// SPDX-FileCopyrightText: 2021 Chorus One AG
// SPDX-License-Identifier: GPL-3.0

//! Utilities for observing a consistent snapshot of on-chain state.
//!
//! The Solana RPC does not have any functionality to query an account at a given
//! block, but it can query multiple accounts at once. Therefore, this module
//! implements an opportunistic way of querying: read all accounts we *expect*
//! to need in one call. If that is all the accounts we really need, then great,
//! we have a consistent view of the on-chain state. If it turns out later that
//! we need to read from an account that is not in our snapshot, then adjust the
//! expected accounts, and retry.
//!
//! There are situations in which this could fail to ever get a useful snapshot.
//! For example, suppose we build a linked list of accounts, where the account's
//! data contains the address of the next account. We want to have a snapshot of
//! the list. If an external process keeps modifying the list, then every time
//! we get a new snapshot, we may find that the tail points to an account that
//! wasn’t yet included in the snapshot, so we retry. But by then, the external
//! process has already modified the tail again, so we are stuck in a loop.
//!
//! This is a pathological example though, for Solido we expect retries to be
//! rare, and when they do happen, they shouldn’t happen repeatedly.

use std::collections::{HashMap, HashSet};

use anchor_lang::AccountDeserialize;
use solana_client::client_error::{ClientError, ClientErrorKind};
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_request::RpcError;
use solana_sdk::account::Account;
use solana_sdk::borsh::try_from_slice_unchecked;
use solana_sdk::program_pack::{IsInitialized, Pack};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use solana_sdk::sysvar::stake_history::StakeHistory;
use solana_sdk::sysvar::{
    self, clock::Clock, recent_blockhashes::RecentBlockhashes, rent::Rent, Sysvar,
};
use solana_sdk::transaction::Transaction;

use lido::state::Lido;
use lido::token::Lamports;
use spl_token::solana_program::hash::Hash;

use crate::error::{Error, MissingAccountError, SerializationError};

pub enum SnapshotError {
    /// We tried to access an account, but it was not present in the snapshot.
    ///
    /// When this happens, we need to retry, with a new set of accounts.
    MissingAccount,

    /// An error occurred that was not related to account lookup in the snapshot.
    ///
    /// When this happens, we need to abort trying to get the snapshot, and we
    /// just return this error.
    OtherError(Error),
}

impl<T> From<T> for SnapshotError
where
    Error: From<T>,
{
    fn from(err: T) -> SnapshotError {
        SnapshotError::OtherError(Error::from(err))
    }
}

pub type Result<T> = std::result::Result<T, SnapshotError>;

/// A set that preserves insertion order.
pub struct OrderedSet<T> {
    // Invariant: the vec and set contain the same elements.
    pub elements_vec: Vec<T>,
    pub elements_set: HashSet<T>,
}

impl<T: std::hash::Hash + Copy + Eq> OrderedSet<T> {
    pub fn new() -> OrderedSet<T> {
        OrderedSet {
            elements_vec: Vec::new(),
            elements_set: HashSet::new(),
        }
    }

    /// Append an element at the end, if it was not yet in the set.
    pub fn push(&mut self, element: T) {
        let is_new = self.elements_set.insert(element);
        if is_new {
            self.elements_vec.push(element);
        }
    }

    /// Merge `other` into `self`.
    ///
    /// This preserves the order of `self`, and adds additional elements at the
    /// end, in the order of `other`.
    pub fn union_with(&mut self, other: &OrderedSet<T>) {
        for element in other.iter() {
            self.push(*element)
        }
    }
}

// Deref impl so we get `.len()`, `.iter()`, `.chunks()`, etc.
// This is the same Deref impl that `Vec` has.
impl<T> std::ops::Deref for OrderedSet<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        self.elements_vec.deref()
    }
}

/// A snapshot of one or more accounts.
pub struct Snapshot<'a> {
    /// Addresses, and their values, at the time of the snapshot.
    ///
    /// The value holds an `Option`, so we can distinguish two "absent" cases:
    ///
    /// * The key is present but the value is `None`. This means that the
    ///   account does not exist on the network, this is an error.
    /// * The key is not present. This means that we did not include it in the
    ///   snapshot, so we need to retry.
    accounts: &'a HashMap<Pubkey, Option<Account>>,

    /// The accounts referenced so far, in the order of first reference.
    ///
    /// This set serves two purposes:
    ///
    /// * If we try to access an account that is not in the set, we can union
    ///   the set of accounts to query with this, so the account is present in
    ///   the next iteration.
    ///
    /// * After a successful run, we can prune the accounts to query, to remove
    ///   any accounts in the snapshot that we did not reference.
    accounts_referenced: &'a mut OrderedSet<Pubkey>,

    /// The wrapped client, so we can still send transactions.
    rpc_client: &'a RpcClient,

    /// Whether we sent at least one transaction.
    ///
    /// If we did, then retrying is potentially unsafe, because it would also
    /// retry sending the transaction. If that happens, we need to update the
    /// program, so it reads everything it needs before sending a transaction.
    sent_transaction: &'a mut bool,
}

impl<'a> Snapshot<'a> {
    /// Return whether an account with the given address exists.
    pub fn account_exists(&mut self, address: &Pubkey) -> Result<bool> {
        self.accounts_referenced.push(*address);
        match self.accounts.get(address) {
            // The account was included in the snapshot, and if it is Some, it
            // did exist.
            Some(account) => Ok(account.is_some()),
            // The account was not included in the snapshot, we do not know if
            // it existed.
            None => Err(SnapshotError::MissingAccount),
        }
    }

    /// Return the account at the given address.
    ///
    /// Fails with `MissingAccountError` if the account does not exist.
    pub fn get_account(&mut self, address: &Pubkey) -> Result<&'a Account> {
        self.accounts_referenced.push(*address);
        match self.accounts.get(address) {
            Some(Some(account)) => Ok(account),
            // The account was included in the snapshot, but it did not exist on
            // the network at the time. This is a fatal error.
            Some(None) => {
                let error: Error = Box::new(MissingAccountError {
                    missing_account: *address,
                });
                Err(error.into())
            }
            // The account was not included in the snapshot, we need to retry.
            None => Err(SnapshotError::MissingAccount),
        }
    }

    /// Read an account and immediately bincode-deserialize it.
    pub fn get_bincode<T: Sysvar>(&mut self, address: &Pubkey) -> Result<T> {
        let account = self.get_account(address)?;
        let result = bincode::deserialize(&account.data)?;
        Ok(result)
    }

    /// Read an Anchor account and immediately deserialize it.
    pub fn get_account_deserialize<T: AccountDeserialize>(
        &mut self,
        address: &Pubkey,
    ) -> Result<T> {
        let account = self.get_account(address)?;
        let mut data_ref = &account.data[..];
        let result = T::try_deserialize(&mut data_ref)?;
        Ok(result)
    }

    /// Read an account, deserialize it with `solana_program_pack`.
    pub fn get_unpack<T: Pack + IsInitialized>(&mut self, address: &Pubkey) -> Result<T> {
        let account = self.get_account(address)?;
        let result = T::unpack(&account.data[..])?;
        Ok(result)
    }

    /// Read `sysvar::rent`.
    pub fn get_rent(&mut self) -> Result<Rent> {
        self.get_bincode(&sysvar::rent::id())
    }

    /// Read `sysvar::clock`.
    pub fn get_clock(&mut self) -> Result<Clock> {
        self.get_bincode(&sysvar::clock::id())
    }

    /// Read `sysvar::stake_history`.
    pub fn get_stake_history(&mut self) -> Result<StakeHistory> {
        self.get_bincode(&sysvar::stake_history::id())
    }

    /// Read `sysvar::recent_blockhashes`.
    pub fn get_recent_blockhashes(&mut self) -> Result<RecentBlockhashes> {
        self.get_bincode(&sysvar::recent_blockhashes::id())
    }

    /// Return the most recent block hash at the time of the snapshot.
    pub fn get_recent_blockhash(&mut self) -> Result<Hash> {
        let blockhashes = self.get_recent_blockhashes()?;
        // The blockhashes are ordered from most recent to least recent.
        Ok(blockhashes[0].blockhash)
    }

    /// Return the minimum rent-exempt balance for an account with `data_len` bytes of data.
    pub fn get_minimum_balance_for_rent_exemption(&mut self, data_len: usize) -> Result<Lamports> {
        let rent = self.get_rent()?;
        Ok(Lamports(rent.minimum_balance(data_len)))
    }

    /// Read the account and deserialize the Solido struct.
    pub fn get_solido(&mut self, solido_address: &Pubkey) -> Result<Lido> {
        let account = self.get_account(solido_address)?;
        match try_from_slice_unchecked::<Lido>(&account.data) {
            Ok(solido) => Ok(solido),
            Err(err) => {
                let error: Error = Box::new(SerializationError {
                    cause: err.into(),
                    address: *solido_address,
                    context: format!(
                        "Failed to deserialize Lido struct, data length is {} bytes.",
                        account.data.len()
                    ),
                });
                Err(error.into())
            }
        }
    }

    /// Return the amount in an SPL token account.
    pub fn get_spl_token_balance(&mut self, address: &Pubkey) -> Result<u64> {
        let account: spl_token::state::Account = self.get_unpack(address)?;
        Ok(account.amount)
    }

    /// Send a transaction without printing to stdout.
    ///
    /// After this, avoid reads from accounts not accessed before. Note, you
    /// probably want to use [`SnapshotConfig::sign_and_send_transaction`]
    /// instead of calling this directly, to ensure correct output handling.
    pub fn send_and_confirm_transaction(
        &mut self,
        transaction: &Transaction,
    ) -> solana_client::client_error::Result<Signature> {
        *self.sent_transaction = true;
        self.rpc_client.send_and_confirm_transaction(transaction)
    }

    /// Send a transaction, show a spinner on stdout.
    ///
    /// After this, avoid reads from accounts not accessed before. Note, you
    /// probably want to use [`SnapshotConfig::sign_and_send_transaction`]
    /// instead of calling this directly, to ensure correct output handling.
    pub fn send_and_confirm_transaction_with_spinner(
        &mut self,
        transaction: &Transaction,
    ) -> solana_client::client_error::Result<Signature> {
        *self.sent_transaction = true;
        self.rpc_client
            .send_and_confirm_transaction_with_spinner(transaction)
    }
}

/// A wrapper around [`RpcClient`] that enables reading consistent snapshots of multiple accounts.
pub struct SnapshotClient {
    rpc_client: RpcClient,

    /// The initial set of accounts to query.
    ///
    /// We store the set here to reuse it between `with_snapshot` calls, so that
    /// a next call doesn't need to learn from scratch which accounts we are
    /// going to access.
    accounts_to_query: OrderedSet<Pubkey>,

    /// The maximum number of accounts that we can request per `GetMultipleAccounts` call.
    ///
    /// This is an empirical observation: initially we set it to `usize::MAX`,
    /// and when we get a too-many-accounts error when requesting `n` accounts,
    /// we set this to `n - 1`, so we should quickly learn an upper bound.
    max_items_per_call: usize,
}

/// Return whether a call to `GetMultipleAccounts` failed due to the RPC account limit.
///
/// If this happens, the RPC operator must increase `--rpc-max-multiple-accounts`
/// on their validator. At the time of writing, it defaults to 100.
fn is_too_many_inputs_error(error: &ClientError) -> bool {
    match error.kind() {
        ClientErrorKind::RpcError(RpcError::RpcRequestError(message)) => {
            // Unfortunately, there is no way to get a structured error; all we
            // get is a string that looks like this:
            //
            //     Failed to deserialize RPC error response: {"code":-32602,
            //     "message":"Too many inputs provided; max 100"} [missing field `data`]
            //
            // So we have to resort to testing for a substring, and if Solana
            // ever changes their responses, this will break :/
            message.contains("Too many inputs provided")
        }
        _ => false,
    }
}

impl SnapshotClient {
    pub fn new(rpc_client: RpcClient) -> SnapshotClient {
        SnapshotClient {
            rpc_client,
            accounts_to_query: OrderedSet::new(),
            max_items_per_call: usize::MAX,
        }
    }

    /// Call `GetMultipleAccounts` to get `self.accounts_to_query`.
    ///
    /// Ideally, we do a single `GetMultipleAccounts` call for the accounts we
    /// need, and then we have a consistent snapshot. But unfortunately, the
    /// default limit on the number of accounts that you can query in one call
    /// is quite low. This means that in somme cases, we may need to resort to
    /// doing multiple calls. This can result in torn reads, and observing an
    /// inconsistent state, but unfortunately there is no other way. If this
    /// happens, we print a warning to stderr.
    ///
    /// Uses the known upper bound on the number of items that we can get per
    /// call, `max_items_per_call` (set to `usize::MAX` initially, when this is
    /// unknown). If we learn a tighter upper bound, this function updates the
    /// maximum.
    fn get_multiple_accounts_chunked(
        &mut self,
    ) -> std::result::Result<Vec<Option<Account>>, crate::error::Error> {
        let mut result = Vec::new();

        // Handle the empty case first, because otherwise we try to make chunks
        // of length 0 below.
        if self.accounts_to_query.is_empty() {
            return Ok(result);
        }

        'num_chunks: for num_chunks in 1.. {
            result.clear();

            let items_per_chunk = self.accounts_to_query.len() / num_chunks;
            assert!(
                items_per_chunk > 0,
                "We should be able to get at least *one* account with GetMultipleAccounts."
            );

            if items_per_chunk > self.max_items_per_call {
                // We already know that this would fail, try again with more chunks.
                continue;
            }

            for chunk in self.accounts_to_query.chunks(items_per_chunk) {
                match self.rpc_client.get_multiple_accounts(chunk) {
                    Ok(accounts) => {
                        result.extend(accounts);
                    }
                    Err(ref err) if is_too_many_inputs_error(err) => {
                        self.max_items_per_call = chunk.len() - 1;
                        continue 'num_chunks;
                    }
                    Err(err) => return Err(err.into()),
                };
            }

            assert_eq!(result.len(), self.accounts_to_query.len());

            // Warn every time if this was not a consistent read, but only warn
            // once per successful read.
            if num_chunks > 1 {
                eprintln!(
                    "Warning: Failed to retrieve all accounts in a single \
                        GetMultipleAccounts call. The resulting snapshot may be \
                        inconsistent."
                );
                eprintln!(
                    "Please ask the RPC node operator to bump \
                        --rpc-max-multiple-accounts to {}, or connect to a \
                        different RPC node.",
                    self.accounts_to_query.len()
                );
            }

            return Ok(result);
        }

        unreachable!("Above loop fails the assertion when items_per_chunk > accounts_to_query.len");
    }

    /// Run the function `f`, which has access to a consistent snapshot of accounts.
    ///
    /// If `f` tries to access an account that's not in the snapshot, we will
    /// retry with an extended snapshot. This means that `f` can be called
    /// multiple times, beware of side effects! In particular, after sending a
    /// transaction, `f` should not try to access any accounts that it did not
    /// access before sending the transaction. For sending transactions, this
    /// function will detect that and panic, but for external side effects (such
    /// as printing to stdout), we can’t, so be careful.
    ///
    /// For the first iteration, the accounts that we load are the ones from the
    /// previous call. This means that it's better to recycle one snapshot client,
    /// than to create a new one all the time.
    pub fn with_snapshot<T, F>(&mut self, mut f: F) -> std::result::Result<T, crate::error::Error>
    where
        F: FnMut(Snapshot) -> Result<T>,
    {
        loop {
            let account_values = self.get_multiple_accounts_chunked()?;
            let accounts: HashMap<_, _> = self
                .accounts_to_query
                .iter()
                .cloned()
                .zip(account_values)
                .collect();

            let mut accounts_referenced = OrderedSet::new();
            let mut sent_transaction = false;

            let snapshot = Snapshot {
                accounts: &accounts,
                accounts_referenced: &mut accounts_referenced,
                rpc_client: &self.rpc_client,
                sent_transaction: &mut sent_transaction,
            };

            match f(snapshot) {
                Ok(result) => {
                    // This snapshot was good, it contained all accounts
                    // referenced by `f`. But it might have contained more. To
                    // prevent the set of accounts from growing indefinitely with
                    // accounts that were once referenced, but now no longer
                    // needed, update our accounts to query to be only what `f`
                    // actually used this time.
                    self.accounts_to_query = accounts_referenced;
                    return Ok(result);
                }
                Err(SnapshotError::OtherError(err)) => return Err(err),
                Err(SnapshotError::MissingAccount) => {
                    if sent_transaction {
                        // `f` tried to access an account that was not in the
                        // snapshot, after already sending a transaction. We
                        // can't just retry, because it would also send that
                        // transaction again. This is a programming error.
                        panic!(
                            "Tried to read an account that is not in the snapshot, \
                            after sending a transaction. Move the read before the \
                            write, or make a new snapshot after the write."
                        );
                    } else {
                        // `f` tried to access an account that was not in the snapshot.
                        // That should have put the account in `accounts_referenced`,
                        // so on the next iteration, we will include that account.
                        // Don't just replace `accounts_to_query` with
                        // `accounts_referenced` though, union them. This way, if we
                        // had a good set for a few snapshots, but now a new account
                        // appears in the middle, we don't throw away those accounts
                        // that we knew we would need later anyway. Merge the old
                        // set into the referenced accounts to preserve the most
                        // recent reference order. This ensures that if we do need
                        // to break up the query into multiple chunks, the accounts
                        // that get referenced after each other will likely end up
                        // in the same chunk, and this minimizes bad effects of
                        // tearing.
                        accounts_referenced.union_with(&self.accounts_to_query);
                        self.accounts_to_query = accounts_referenced;
                    }
                }
            }
        }
    }
}
