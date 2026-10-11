# AGENTS.md

Notes for coding agents (and humans) working on spr.

spr is a command-line tool that turns local Git commits into pull requests: one pull request per commit, stackable, and friendly to amending and rebasing. It's written in Rust. It works with GitHub (github.com and GitHub Enterprise), behind a forge interface meant for more forges.

- Code: a single crate, `spr` (`src/`), plus the live tests in `livetest/` (a separate package in the workspace).
- User documentation: an mdBook in `docs/` (`docs/SUMMARY.md`), published at https://spacedentist.github.io/spr/.
- Coding style, structure of the code, error handling, testing: see [CodingGuidelines.md](CodingGuidelines.md).

## Commands

```shell
cargo build
cargo test                       # spr's unit and integration tests
cargo test --workspace           # also the live tests' own unit tests
cargo fmt --all                  # max_width = 80 (rustfmt.toml)
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

CI runs clippy with all warnings as errors, checks formatting, runs `cargo test --workspace --all-features`, and builds with the minimum supported Rust version (`rust-version` in `Cargo.toml`).

`shell.nix` (with direnv: `use nix`) provides all tools, including the formatters for Markdown and YAML (prettier), TOML (taplo) and Nix (nixfmt), and pre-commit. The pre-commit hooks (`.pre-commit-config.yaml`) run the formatters and clippy; run `pre-commit run --all-files` if they aren't installed. If your environment doesn't load direnv, run commands in the Nix shell with `nix-shell --run '…'` — a system Rust toolchain may be older than the one CI uses, and miss lints that CI reports.

## Conventions

- Keep unrelated changes in separate commits. Format code in the commit that introduces it — no formatting-only commits.
- Commit messages: a short title in the imperative mood, and a body that explains what changed and why. Don't add co-author lines for AI tools or models.
- User-facing changes get an entry under "Unreleased" in `CHANGELOG.md`. New configuration options go into `docs/reference/configuration.md`, and changes in behaviour into the user documentation in `docs/`.
- Don't commit or push unless asked to.

## Design principles

- **The local branch is the source of truth.** spr projects each local commit onto its pull request branch, and only adds trailers to local commit messages (`Pull-request:`, and local-only ones like `Spr-Id:` and `Spr-Target:`).
- **spr never force-pushes pull request branches.** It adds commits to them (merging in the target branch or the parent's pull request as needed). It must cope with branches that others changed (e.g. with GitHub's "Update branch" button).
- **spr only deletes branches it created**: base branches (`Config::is_spr_base_branch`: `{prefix}{target branch}.{slug}`), never another pull request's branch. Before deleting a branch, it retargets the pull requests based on it.
- **Configuration:** `spr.target` (e.g. `github:owner/repo:main`, see `src/target.rs`), tokens in `spr.auth.<forge>[.<host>].token`, `spr.mergeMethod` (`squash` | `merge`), `spr.stackingMode` (`base-branches` | `chain` | `github-stack`). `merge` with `base-branches` is rejected. Deprecated options are still read, with a note on how to replace them.

## GitHub API quirks

- The stacks API and `merge-async` need `X-GitHub-Api-Version: 2026-03-10`. octocrab's `build_request` adds its own default version header, which has to be replaced (see `GitHub::stacks_api_request`).
- GitHub refuses to change the base of a pull request that is in a stack: dissolve the stack first. Stacked pull requests can only be merged via `PUT …/pulls/{n}/merge-async`.
- After a push, GitHub takes a few seconds to update a pull request's diff and mergeability.
- octocrab puts every request path below the path of its base address. On GitHub Enterprise Server, the GraphQL API (`/api/graphql`) isn't below the REST API (`/api/v3`), so spr uses a separate client for it (see `ApiUrls`).
- GitHub Enterprise has no stacked pull requests: `github-stack` is github.com only.

## Testing

- Unit tests run anywhere, without network access: `TestRepo` (`src/test_utils.rs`) builds commit graphs in a temporary repository, and API requests are tested against a mock server (wiremock).
- The live tests (`livetest/`, `cargo run -p spr-livetest -- --help`) run spr against a real GitHub test repository. They need a token in `SPR_GITHUB_LIVETEST_TOKEN` and a repository meant for testing (with a `.spr-livetest` file). Only run them when the user provides both; never take a token on your own (e.g. from the GitHub CLI).
- See [docs/dev/testing.md](docs/dev/testing.md) for details.

## Security

- Tokens must never appear in output, logs, error messages or command lines (see [CodingGuidelines.md](CodingGuidelines.md#secrets)).
