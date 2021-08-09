# Lido for Solana

[![Coverage][cov-img]][cov]

[cov-img]: https://codecov.io/gh/ChorusOne/solido/branch/main/graph/badge.svg?token=USB921ZL6B
[cov]:     https://codecov.io/gh/ChorusOne/solido/branch/main/graph/badge.svg?token=USB921ZL6B

Lido for Solana is a [Lido DAO][lido]-governed liquid staking protocol for the
Solana blockchain. Anyone who stakes their SOL tokens with Lido will be issued
an on-chain representation of the SOL staking position with Lido validators,
called stSOL.

Lido for Solana gives you:

 * **Liquidity** — No delegation/activation delays and the ability to sell your
   staked tokens
 * **One-click staking** — No complicated steps
 * **Decentralized security** — Assets spread across the industry’s leading
   validators chosen by the Lido DAO

[lido]: https://lido.fi

## Further reading

   <!-- TODO: Update link to staking page once we are live on mainnet. -->
 * [Staking page for end users (devnet)][stake]
 * [Documentation][documentation]
 * [Blog][blog]

[stake]:         https://solana-dev.testnet.lido.fi/
[documentation]: https://chorusone.github.io/solido/
[blog]:          https://medium.com/chorus-one

## Repository layout

This repository contains:

 * `program` — The on-chain Solana BPF program.
 * `multisig` — A pinned version of the [Serum multisig program][multisig], used
   as the upgrade authority of the deployed Solido program, and as the manager
   of the Solido instance.


This repository contains both the Solana program (./program) and a client (./cli) for interacting with
the program.

[multisig]: https://github.com/project-serum/multisig

## License

Solido is licensed under the GNU General Public License version 3.
