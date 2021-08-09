# Lido for Solana

[![Coverage][cov-img]][cov]

[cov-img]: https://codecov.io/gh/ChorusOne/solido/branch/main/graph/badge.svg?token=USB921ZL6B
[cov]:     https://codecov.io/gh/ChorusOne/solido/branch/main/graph/badge.svg?token=USB921ZL6B

*Lido for Solana* (“Solido” for short) is a [Lido DAO][lido]-governed liquid
staking protocol for the Solana blockchain. Anyone who stakes their SOL tokens
with Lido will be issued an on-chain representation of the SOL staking position
with Lido validators, called *stSOL*.

Lido for Solana gives you:

 * **Liquidity** — No delegation/activation delays and the ability to sell your
   staked tokens
 * **One-click staking** — No complicated steps
 * **Decentralized security** — Assets spread across the industry’s leading
   validators chosen by the Lido DAO

Further resources:

   <!-- TODO: Update link to staking page once we are live on mainnet. -->
 * [Staking page for end users (devnet)][stake]
 * [Documentation][documentation]
 * [Blog][blog]

[lido]:          https://lido.fi
[stake]:         https://solana-dev.testnet.lido.fi/
[documentation]: https://chorusone.github.io/solido/
[blog]:          https://medium.com/chorus-one

## Repository layout

This repository contains the source code for the on-chain program, and for the
`solido` utility to interact with it. The source code for the staking widget,
and documentation, are in a different repository, which is not yet public.

 * `program` — The on-chain Solana BPF program.
 * `multisig` — A pinned version of the on-chain [Serum multisig
   program][multisig], used as the upgrade authority of the Solido program, and
   as the manager of the Solido instance.
 * `cli` — The command-line `solido` utility for interacting with the on-chain
   programs.
 * `docker` — Dockerfiles for reproducible builds, and for the maintainer image.
 * `tests` — Scripts that test the actual `solido` binary and on-chain program.
 * `program/tests` — Tests using the `solana-program-test` test framework.

[multisig]: https://github.com/project-serum/multisig

## Building

The on-chain program and `solido` utility are written in Rust. To build them,
you need:

 * [A Rust toolchain][rust]
 * [The Solana tool suite][solana-tools] (only needed for the on-chain programs,
   not for the `solido` utility)
 * [Docker][docker] (only needed if you want to [reproduce][reproduce] the
   official build, or if you want to avoid installing build tools locally)
 * The following system libraries (listed as Debian package names):
   * `libudev-dev`
   * `libhidapi-dev`
   * `pkg-config`
   * `openssl`

The Solana version that we test against is listed in our [CI config][ci-config].

[rust]:         https://www.rust-lang.org/tools/install
[solana-tools]: https://docs.solana.com/cli/install-solana-cli-tools
[docker]:       https://docs.docker.com/engine/install/
[reproduce]:    https://chorusone.github.io/solido/development/reproducibility/
[ci-config]:    https://github.com/ChorusOne/solido/blob/main/.github/workflows/build.yml

### Solido utility

To build and test the `solido` utility, use the normal Cargo commands:

```console
$ cargo test
$ cargo build --release
```

The `solido` binary can then be found in `target/release`.

### On-chain programs

Building the on-chain programs requires [the Solana tool suite][solana-tools]:

```console
$ cargo build-bpf
$ cargo test-bpf
```

The programs `lido.so` and `multisig.so` can then be found in `target/deploy`.

### Docker container

To build the local image, use `buildimage.sh`. This will build and package
Solido along with the Solana toolchain into an image `chorusone/solido:hash`,
where _hash_ will be the Git hash of the current version of the codebase.

Once built, one can execute into the container interactively:

```console
$ docker run --interactive --tty --rm chorusone/solido:hash /bin/sh
```

This will provide a shell into the working directory where the Solido artefacts
and the Solana toolchain are located. Inside that directory, the the `solido`
utility is in `solido/cli`, and the on-chain programs are in `solido/deploy`.

## License

Lido for Solana is licensed under the GNU General Public License version 3.
