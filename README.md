![spr](./docs/spr.svg)

# spr &middot; [![GitHub](https://img.shields.io/github/license/getcord/spr)](https://img.shields.io/github/license/getcord/spr) [![GitHub release](https://img.shields.io/github/v/release/getcord/spr?include_prereleases)](https://github.com/getcord/spr/releases) [![crates.io](https://img.shields.io/crates/v/spr.svg)](https://crates.io/crates/spr) [![GitHub Repo stars](https://img.shields.io/github/stars/getcord/spr?style=social)](https://github.com/getcord/spr)

A command-line tool for submitting and updating GitHub Pull Requests from local
Git commits that may be amended and rebased. Pull Requests can be stacked to
allow for a series of code reviews of interdependent code.

spr is pronounced /ˈsuːpəɹ/, like the English word 'super'.

## Installation

### Binary Installation

#### Using Homebrew

```shell
brew install getcord/tap/spr
```

#### Using Cargo

If you have Cargo installed (the Rust build tool), you can install spr by running `cargo install spr`.

### Install from Source

spr is written in Rust. You need a Rust toolchain to build from source. See [rustup.rs](https://rustup.rs) for information on how to install Rust if you have not got a Rust toolchain on your system already.

With Rust all set up, clone this repository and run `cargo build --release`. The spr binary will be in the `target/release` directory.

## Quickstart

To use spr, run `spr init` inside a local checkout of a GitHub-backed git repository. You will be asked for a GitHub PAT (Personal Access Token), which spr will use to make calls to the GitHub API in order to create and merge pull requests.

To submit a commit for pull request, run `spr diff`.

If you want to make changes to the pull request, amend your local commit (and/or rebase it) and call `spr diff` again. When updating an existing pull request, spr will ask you for a short message to describe the update.

To squash-merge an open pull request, run `spr land`.

For more information on spr commands and options, run `spr help`. For more information on a specific spr command, run `spr help <COMMAND>` (e.g. `spr help diff`).

## Documentation

There is no more documentation at this point. This README will be updated as soon as there are additional resources.

## Contributing

Feel free to submit an issue on [GitHub](https://github.com/getcord/spr) if you have found a problem. If you can even provide a fix, please raise a pull request!

If there are larger changes or features that you would like to work on, please raise an issue on GitHub first to discuss.

### License

spr is [MIT licensed](./LICENSE).
