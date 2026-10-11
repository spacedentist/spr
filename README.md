![spr](./docs/spr.svg)

# spr &middot; [![GitHub](https://img.shields.io/github/license/spacedentist/spr)](./LICENSE) [![GitHub release](https://img.shields.io/github/v/release/spacedentist/spr?include_prereleases)](https://github.com/spacedentist/spr/releases) [![crates.io](https://img.shields.io/crates/v/spr.svg)](https://crates.io/crates/spr) [![homebrew](https://img.shields.io/homebrew/v/spr.svg)](https://formulae.brew.sh/formula/spr) [![GitHub Repo stars](https://img.shields.io/github/stars/spacedentist/spr?style=social)](https://github.com/spacedentist/spr)

A command-line tool for submitting and updating GitHub Pull Requests from local
Git commits that may be amended and rebased. Pull Requests can be stacked to
allow for a series of code reviews of interdependent code.

spr is pronounced /ˈsuːpəɹ/, like the English word 'super'.

## Migrating to spr 2.0

spr 2.0 is available as a [pre-release](#pre-releases). Coming from spr 1.x, these changes may affect you:

- **spr's fields in commit messages are Git trailers now.** Git treats `Key: value` lines in the last paragraph of a commit message as [trailers](https://git-scm.com/docs/git-interpret-trailers), like `Signed-off-by:`, and tools can read them (e.g. `git log --format='%(trailers)'`). spr 1.x wrote `Pull Request:`, `Reviewers:` and `Reviewed By:` fields, which weren't trailers: keys can't contain spaces. spr 2.0 writes the trailers `Pull-request:`, `Reviewers:` and `Reviewed-by:`. It still recognises the old fields, and converts them when it next updates a commit message.
- **No more `Summary:` and `Test Plan:` sections.** The body of the commit message is the pull request's description, as it is. The option `spr.requireTestPlan` is gone.
- **spr doesn't run `git` anymore.** It fetches and pushes directly to the repository's HTTPS URL, with its GitHub token (pushing via SSH, e.g. with `pushInsteadOf`, still works). The option `spr.githubRemoteName` is gone.
- **Configuration options have new names.** spr still reads the old ones for now, and shows a note on how to replace them. `spr init` replaces them in the repository's configuration.

  | spr 1.x                                             | spr 2.0                                                                       |
  | --------------------------------------------------- | ----------------------------------------------------------------------------- |
  | `spr.githubRepository` and `spr.githubMasterBranch` | `spr.target`, e.g. `github:owner/repo:main`                                   |
  | `spr.githubAuthToken`                               | `spr.auth.github.token` (for GitHub Enterprise: `spr.auth.github.HOST.token`) |
  | `--github-repository` and `--github-master-branch`  | `--target`                                                                    |
  | `--github-auth-token`                               | `--auth-token`                                                                |

- **Building spr needs Rust 1.91** or newer.

See the [changelog](./CHANGELOG.md) for everything that's new.

## Documentation

Comprehensive documentation is available here: https://spacedentist.github.io/spr/

## Installation

[![Packaging status](https://repology.org/badge/vertical-allrepos/spr-super-pull-requests.svg)](https://repology.org/project/spr-super-pull-requests/versions)

### Binary Installation

#### Using Homebrew

```shell
brew install spr
```

#### Using Nix

spr is available in nixpkgs

```shell
nix run nixpkgs#spr
```

For the latest development version, use the flake in this repository:

```shell
nix run github:spacedentist/spr
```

#### Using Cargo

If you have Cargo installed (the Rust build tool), you can install spr by running

```shell
cargo install spr
```

### Pre-releases

Pre-releases are on [GitHub](https://github.com/spacedentist/spr/releases) and crates.io. Homebrew, nixpkgs and a plain `cargo install spr` stay with the latest stable release. To install a pre-release, give its version (e.g. `2.0.0-beta.1`):

```shell
cargo install spr --version 2.0.0-beta.1
nix run github:spacedentist/spr/v2.0.0-beta.1
```

### Install from Source

See [Development and building from source](#development-and-building-from-source) below.

## Quickstart

To use spr, run `spr init` inside a local checkout of a GitHub-backed git repository. You will be guided through authorising spr to use the GitHub API in order to create and merge pull requests.

To submit a commit for review as a pull request, run `spr diff`.

If you want to make changes to the pull request, amend your local commit (and/or rebase it) and call `spr diff` again. When updating an existing pull request, spr will ask you for a short message to describe the update.

With several commits on your branch, `spr diff --all` creates or updates a pull request for each of them, stacked on top of each other. See [Stack Multiple PRs](https://spacedentist.github.io/spr/user/stack.html) for the workflow.

To land an approved pull request, run `spr land` (it squash-merges by default, see [Choose a Merge Method and Stacking Mode](https://spacedentist.github.io/spr/user/stacking-modes.html)).

For more information on spr commands and options, run `spr help`. For more information on a specific spr command, run `spr help <COMMAND>` (e.g. `spr help diff`).

## Development and building from source

spr is written in Rust. To build it, you need:

- Rust 1.91 or newer. See [rustup.rs](https://rustup.rs) for how to install it.
- A C compiler, `pkg-config`, and the OpenSSL development files (e.g. `libssl-dev` on Debian and Ubuntu, `openssl-devel` on Fedora, `openssl` from Homebrew on macOS). libgit2 and libssh2 are built from source as part of the build, unless matching versions are installed on your system.

Clone this repository and run `cargo build --release`. The spr binary will be in the `target/release` directory. Run the tests with `cargo test`.

For working on spr, you also need:

- `rustfmt` and `clippy` (with rustup: `rustup component add rustfmt clippy`). CI checks that `cargo fmt --all --check` passes, that `cargo clippy --workspace --all-targets --all-features` gives no warnings, and runs `cargo test --workspace`. (`--workspace` includes the live tests, see below; without it, cargo only builds and tests spr itself.)
- Formatters for the other files (CI doesn't check these yet): [prettier](https://prettier.io) for Markdown and YAML, [taplo](https://taplo.tamasfe.dev) for TOML, and [nixfmt](https://github.com/NixOS/nixfmt) for Nix files.
- Optionally [pre-commit](https://pre-commit.com): `pre-commit install` sets up a Git hook that runs all of the above formatters and clippy when you commit (`.pre-commit-config.yaml`). It uses the tools installed on your system.
- For the documentation (in `docs/`): [mdBook](https://rust-lang.github.io/mdBook/) and [mdbook-mermaid](https://github.com/badboy/mdbook-mermaid). Run `mdbook-mermaid install` once, then `mdbook serve` to view it.

### Tests

`cargo test` runs the unit and integration tests, which need nothing but a local Git repository. The live tests in `livetest/` run spr against a GitHub repository meant for testing, and need a GitHub token; they and the guided manual tests (e.g. of logging in) run on demand, not in CI:

```shell
export SPR_GITHUB_LIVETEST_TOKEN=$(gh auth token)  # or a token of your own
cargo run -p spr-livetest -- --repo OWNER/REPO live          # all automated live tests
cargo run -p spr-livetest -- --repo OWNER/REPO manual init  # guided test of logging in
```

See [Testing](https://spacedentist.github.io/spr/dev/testing.html) in the documentation for what each kind of test covers, what the live tests need and do in the repository, and what CI runs.

### Nix

With Nix, `nix-shell` (or [direnv](https://direnv.net) with `use nix` in `.envrc`) gives you a shell with all of these tools, from your system's nixpkgs.

The minimum Rust version is what spr and its dependencies need. We raise it when needed, to any version that is at least about six months old.

## Contributing

Feel free to submit an issue on [GitHub](https://github.com/spacedentist/spr) if you have found a problem. If you can even provide a fix, please raise a pull request!

If there are larger changes or features that you would like to work on, please raise an issue on GitHub first to discuss.

See [AGENTS.md](./AGENTS.md) for an overview of the code base and its conventions, and [CodingGuidelines.md](./CodingGuidelines.md) for the coding style.

### AI Coding Tools

If you use AI coding tools, please read and understand all code changes before submitting them as a PR. Please refrain from listing your AI coding tools or models as co-contributors. Only submit PRs that you are happy to give your name to and be responsible for.

### License

spr is [MIT licensed](./LICENSE).
