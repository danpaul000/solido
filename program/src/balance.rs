// SPDX-FileCopyrightText: 2021 Chorus One AG
// SPDX-License-Identifier: GPL-3.0

//! Logic for keeping the stake pool balanced.

use std::ops::Mul;

use crate::account_map::PubkeyAndEntry;
use crate::state::{Validator, Validators};
use crate::{
    error::LidoError,
    token,
    token::{Lamports, Rational},
};

/// Compute the ideal stake balance for each validator.
///
/// The validator order in the result is the same as in `current_balance`.
///
/// This function targets a uniform distribution over all active validators.
pub fn get_target_balance(
    undelegated_lamports: Lamports,
    validators: &Validators,
) -> Result<Vec<Lamports>, LidoError> {
    let total_delegated_lamports: token::Result<Lamports> = validators
        .iter_entries()
        .map(|v| v.stake_accounts_balance)
        .sum();

    let total_lamports = total_delegated_lamports.and_then(|t| t + undelegated_lamports)?;

    // We only want to target validators that are not in the process of being
    // removed.
    let num_active_validators = validators.iter_active().count() as u64;

    // No active validators.
    if num_active_validators == 0 {
        return Err(LidoError::NoActiveValidators);
    }

    let lamports_per_validator = total_lamports
        .mul(Rational {
            numerator: 1,
            denominator: num_active_validators,
        })
        .expect("Does not divide by zero because `num_active_validators != 0`");

    // Target an uniform distribution.
    let mut target_balance: Vec<Lamports> = validators
        .iter_entries()
        .map(|validator| {
            if validator.active {
                lamports_per_validator
            } else {
                Lamports(0)
            }
        })
        .collect();

    // The total lamports to distribute may be slightly larger than the total
    // lamports we distributed so far, because we round down.
    let total_lamports_distributed = target_balance
        .iter()
        .cloned()
        .sum::<token::Result<Lamports>>()
        .expect("Does not overflow, is at most total_lamports.");

    let mut remainder = (total_lamports - total_lamports_distributed)
        .expect("Does not underflow because we distribute at most total_lamports.");

    assert!(remainder.0 < num_active_validators);

    // Distribute the remainder among the first few active validators, give them
    // one Lamport each. This does mean that the validators early in the list
    // are in a more beneficial position because their stake target is one
    // Lamport higher, but to put that number into perspective, the transaction
    // fee per signature is 10k Lamports at the time of writing. Also, there is
    // a minimum amount we can stake, so in practice, validators will never be
    // as close to their target that the one Lamport matters anyway.
    for (target, validator) in target_balance.iter_mut().zip(validators.iter_entries()) {
        if remainder == Lamports(0) {
            break;
        }
        if validator.active {
            *target = (*target + Lamports(1)).expect(
                "Does not overflow because per-validator balance is at most total_lamports.",
            );
            remainder =
                (remainder - Lamports(1)).expect("Does not underflow due to loop condition.");
        }
    }

    // Sanity check: now we should have distributed all inputs.
    let total_lamports_distributed = target_balance
        .iter()
        .cloned()
        .sum::<token::Result<Lamports>>()
        .expect("Does not overflow, is at most total_lamports.");

    assert_eq!(total_lamports_distributed, total_lamports);

    Ok(target_balance)
}

/// Given a list of validators and their target balance, return the index of the
/// one furthest below its target, and the amount by which it is below.
///
/// This assumes that there is at least one active validator. Panics otherwise.
pub fn get_validator_furthest_below_target(
    validators: &Validators,
    target_balance: &[Lamports],
) -> (usize, Lamports) {
    assert_eq!(
        validators.len(),
        target_balance.len(),
        "Must have as many target balances as current balances."
    );

    // Our initial index, that will be returned when no validator is below its target,
    // is the first active validator.
    let mut index = validators
        .iter_entries()
        .position(|v| v.active)
        .expect("get_validator_furthest_below_target requires at least one active validator.");
    let mut amount = Lamports(0);

    for (i, (validator, target)) in validators.iter_entries().zip(target_balance).enumerate() {
        let amount_below = Lamports(
            target
                .0
                .saturating_sub(validator.effective_stake_balance().0),
        );
        if amount_below > amount {
            amount = amount_below;
            index = i;
        }
    }

    (index, amount)
}

pub fn get_validator_to_withdraw(
    validators: &Validators,
) -> Result<&PubkeyAndEntry<Validator>, crate::error::LidoError> {
    validators
        .entries
        .iter()
        .max_by_key(|v| v.entry.effective_stake_balance())
        .ok_or(LidoError::NoActiveValidators)
}

#[cfg(test)]
mod test {
    use super::{get_target_balance, get_validator_furthest_below_target};
    use crate::state::Validators;
    use crate::token::Lamports;

    #[test]
    fn get_target_balance_works_for_single_validator() {
        // 100 Lamports delegated + 50 undelegated => 150 per validator target.
        let mut validators = Validators::new_fill_default(1);
        validators.entries[0].entry.stake_accounts_balance = Lamports(100);
        let undelegated_stake = Lamports(50);
        let targets = get_target_balance(undelegated_stake, &validators).unwrap();
        assert_eq!(targets[0], Lamports(150));

        // With only one validator, that one is the least balanced. It is
        // missing the 50 undelegated Lamports.
        assert_eq!(
            get_validator_furthest_below_target(&validators, &targets[..]),
            (0, Lamports(50))
        );
    }

    #[test]
    fn get_target_balance_works_for_integer_multiple() {
        // 200 Lamports delegated + 50 undelegated => 125 per validator target.
        let mut validators = Validators::new_fill_default(2);
        validators.entries[0].entry.stake_accounts_balance = Lamports(101);
        validators.entries[1].entry.stake_accounts_balance = Lamports(99);

        let undelegated_stake = Lamports(50);
        let targets = get_target_balance(undelegated_stake, &validators).unwrap();
        assert_eq!(targets, [Lamports(125), Lamports(125)]);

        // The second validator is further away from its target.
        assert_eq!(
            get_validator_furthest_below_target(&validators, &targets[..]),
            (1, Lamports(26))
        );
    }

    #[test]
    fn get_target_balance_works_for_non_integer_multiple() {
        // 200 Lamports delegated + 51 undelegated => 125 per validator target,
        // and one validator gets 1 more.
        let mut validators = Validators::new_fill_default(2);
        validators.entries[0].entry.stake_accounts_balance = Lamports(101);
        validators.entries[1].entry.stake_accounts_balance = Lamports(99);

        let undelegated_stake = Lamports(51);
        let targets = get_target_balance(undelegated_stake, &validators).unwrap();
        assert_eq!(targets, [Lamports(126), Lamports(125)]);

        // The second validator is further from its target, by one Lamport.
        assert_eq!(
            get_validator_furthest_below_target(&validators, &targets[..]),
            (1, Lamports(26))
        );
    }

    #[test]
    fn get_target_balance_already_balanced() {
        let mut validators = Validators::new_fill_default(2);
        validators.entries[0].entry.stake_accounts_balance = Lamports(50);
        validators.entries[1].entry.stake_accounts_balance = Lamports(50);

        let undelegated_stake = Lamports(0);
        let targets = get_target_balance(undelegated_stake, &validators).unwrap();
        assert_eq!(targets, [Lamports(50), Lamports(50)]);

        assert_eq!(
            get_validator_furthest_below_target(&validators, &targets[..]),
            (0, Lamports(0))
        );
    }
    #[test]
    fn get_target_balance_works_with_inactive_for_non_integer_multiple() {
        let mut validators = Validators::new_fill_default(3);
        validators.entries[0].entry.stake_accounts_balance = Lamports(101);
        validators.entries[1].entry.stake_accounts_balance = Lamports(0);
        validators.entries[1].entry.active = false;
        validators.entries[2].entry.stake_accounts_balance = Lamports(99);

        let undelegated_stake = Lamports(51);
        let targets = get_target_balance(undelegated_stake, &validators).unwrap();
        assert_eq!(targets, [Lamports(126), Lamports(0), Lamports(125)]);

        assert_eq!(
            get_validator_furthest_below_target(&validators, &targets[..]),
            (2, Lamports(26))
        );
    }

    #[test]
    fn get_target_balance_works_with_inactive_for_integer_multiple() {
        // 500 Lamports delegated, but only two active validators out of three.
        // All target should be divided equally within the active validators.
        let mut validators = Validators::new_fill_default(3);
        validators.entries[0].entry.stake_accounts_balance = Lamports(100);
        validators.entries[1].entry.stake_accounts_balance = Lamports(100);
        validators.entries[1].entry.active = false;
        validators.entries[2].entry.stake_accounts_balance = Lamports(300);

        let undelegated_stake = Lamports(0);
        let targets = get_target_balance(undelegated_stake, &validators).unwrap();
        assert_eq!(targets, [Lamports(250), Lamports(0), Lamports(250)]);

        assert_eq!(
            get_validator_furthest_below_target(&validators, &targets[..]),
            (0, Lamports(150))
        );
    }

    #[test]
    fn get_target_balance_all_inactive() {
        // No active validators exist.
        let mut validators = Validators::new_fill_default(3);
        validators.entries[0].entry.stake_accounts_balance = Lamports(1);
        validators.entries[1].entry.stake_accounts_balance = Lamports(2);
        validators.entries[2].entry.stake_accounts_balance = Lamports(3);
        validators.entries[0].entry.active = false;
        validators.entries[1].entry.active = false;
        validators.entries[2].entry.active = false;

        let undelegated_stake = Lamports(0);
        let result = get_target_balance(undelegated_stake, &validators);
        assert!(result.is_err());
    }

    #[test]
    fn get_target_balance_no_preference_but_some_inactive() {
        // Every validator is exactly at its target, no validator is below.
        // But the validator furthest below target should still be an active one,
        // not the inactive one.
        let mut validators = Validators::new_fill_default(2);
        validators.entries[0].entry.stake_accounts_balance = Lamports(0);
        validators.entries[1].entry.stake_accounts_balance = Lamports(10);
        validators.entries[0].entry.active = false;

        let undelegated_stake = Lamports(0);
        let targets = get_target_balance(undelegated_stake, &validators).unwrap();
        assert_eq!(
            get_validator_furthest_below_target(&validators, &targets[..]),
            (1, Lamports(0)),
        );
    }
}
