# Testing

spr has four kinds of tests. Most of spr's logic is tested without GitHub, by tests that run anywhere, in seconds, and in CI. What only GitHub can show — how spr's commands work with real pull requests — is tested by live tests against a GitHub repository, which need a token and run on demand. What only a person can do, like logging in through the browser, is covered by guided manual tests.

| kind              | where                               | needs                                | run with                                                     | in CI |
| ----------------- | ----------------------------------- | ------------------------------------ | ------------------------------------------------------------ | ----- |
| unit tests        | `src/` (in the modules)             | nothing                              | `cargo test`                                                 | yes   |
| integration tests | `tests/`                            | nothing                              | `cargo test`                                                 | yes   |
| live tests        | `livetest/` (`spr-github-livetest`) | a GitHub token and a test repository | `cargo run -p spr-livetest -- --repo OWNER/REPO live`        | no    |
| manual tests      | `livetest/` (`spr-github-livetest`) | a GitHub token, and you              | `cargo run -p spr-livetest -- --repo OWNER/REPO manual init` | no    |

## Unit tests

The unit tests sit next to the code they test, in `#[cfg(test)]` modules. They cover everything that doesn't need GitHub:

- **Commit messages** (`message.rs`): parsing and writing titles, descriptions and trailers, including messages written by older spr versions, and which trailers end up in pull requests and landed commits.
- **Constructing pull requests** (`pr_commits.rs`): which commits spr creates on a pull request branch and its base for a given local commit, in all the situations the stacking modes lead to; and the check whether a pull request has changes that aren't in the local commit.
- **Landing** (`land_check.rs`): the check that merging a pull request gives the same result as applying the local commits.
- **Pulling changes others pushed** (`remote_changes.rs`): what `spr pull` does with which history of a pull request.
- **Local Git operations** (`git.rs`): rewriting commit messages safely, and the records spr keeps under `refs/spr/`.
- **Configuration, branch names, `spr init`'s checks, the plumbing commands' options**, and smaller helpers.

Tests that need commits build them in a temporary repository with `TestRepo` (`src/test_utils.rs`), directly as Git objects: `commit`, `commit_tree`, `tree_with` and friends make commit graphs of any shape in a few lines.

The rule of thumb for new code: logic that doesn't need GitHub goes into a function that works on Git objects (like the [plumbing commands](../reference/plumbing.md)), so it can be unit-tested. The porcelain commands then only connect it to GitHub.

## Integration tests

The tests in `tests/` run the spr binary itself. So far they cover the plumbing commands: that they work in a repository without any spr configuration, and their exit codes and JSON error output.

## Live tests

The live tests (`livetest/`, the program `spr-github-livetest`) run spr against a real GitHub repository, the way you use it: they make commits, run spr commands, and check the results on GitHub and in the local repository. There are scenarios for:

- creating, updating and closing a pull request (`basic`)
- landing, by squash-merging and with a merge commit (`land`, `land-merge`)
- changes others push to a pull request: `spr diff` stopping, `--force`, `spr pull`, conflicts, and GitHub's "Update branch" (`remote-changes`, `pull`, `pull-conflict`, `update-branch`)
- stacked pull requests in the three [stacking modes](../user/stacking-modes.md) (`stack-base-branches`, `stack-chain`, `stack-github`)
- `spr patch`, `spr amend`, `spr list` and `spr diff --dry-run` (`patch`, `amend`, `dry-run`)

A full run takes about five minutes.

### What they need

- **A GitHub repository meant for testing.** The tests create and close pull requests and branches in it, so they refuse to run unless its default branch contains a file `.spr-livetest`. Use a dedicated repository, not a real project.
- **A GitHub token**, in the environment variable `SPR_GITHUB_LIVETEST_TOKEN`. The tests never use a token on their own (e.g. the GitHub CLI's), so you always decide which account they act as. To use the GitHub CLI's token:

```shell
export SPR_GITHUB_LIVETEST_TOKEN=$(gh auth token)
```

### Running them

```shell
cargo run -p spr-livetest -- --repo OWNER/REPO live              # all of them
cargo run -p spr-livetest -- --repo OWNER/REPO live pull land    # some of them
cargo run -p spr-livetest -- --repo OWNER/REPO live --list       # what there is
cargo run -p spr-livetest -- --help
```

They build and test spr from your working copy. To test another spr binary, e.g. a release, pass `--spr PATH`.

### What they do in the repository

Each run gets an ID (e.g. `20261008-153012-1a2b`), and works on branches of its own:

- its pull requests' branches are under `spr/livetest/<run-id>/`;
- instead of the default branch, its pull requests target `livetest/<run-id>/main`, created from the default branch at the start. So landing never touches the default branch.

At the end of a run, the tests close its pull requests and delete its branches. If a test fails, `--keep` leaves everything in place (and the test's local directory) for a closer look. To remove what interrupted or kept runs left behind:

```shell
cargo run -p spr-livetest -- --repo OWNER/REPO cleanup [RUN-ID]
```

Cleanup only ever touches branches and pull requests of test runs.

### The controlled environment

Your own Git configuration must not influence the tests (e.g. a `spr.githubAuthToken` in your global configuration). So each test runs spr in a fresh clone, in a temporary directory with a home directory of its own, and with a cleared environment. Before each test, it checks that spr sees no `spr.*` setting from outside that clone. The token never appears on a command line or in the output.

## Manual tests

Some things need a person: logging in with GitHub in a web browser, and checking what GitHub's pages show. Manual tests guide you through them:

```shell
cargo run -p spr-livetest -- --repo OWNER/REPO manual init
```

`manual init` runs `spr init` in a fresh clone without any spr settings, so it starts with logging in. It explains beforehand what you need to do and what to look out for. Afterwards, it checks what it can on its own (the settings `spr init` wrote, and that the new token works and has exactly the scopes spr asks for), asks you about what only you could see (the permissions GitHub's authorisation page listed), tries some variants without your help (a configured token is kept, a broken one replaced, logging in without a browser), and prints a report.

It needs the token like the live tests (for cloning the repository and its checks), but doesn't change anything in the repository, so it doesn't require the `.spr-livetest` file.

## What runs in CI

CI (`.github/workflows/test.yml`) runs on every push to `master` and on every pull request:

- `cargo clippy --workspace --all-targets --all-features`, with all warnings turned into errors
- `cargo fmt --all --check`
- `cargo test --workspace --all-features`: the unit and integration tests, and the unit tests of the live tests' own helpers
- a check that spr builds with the minimum supported Rust version (`rust-version` in `Cargo.toml`)

`--workspace` includes `livetest/`: CI doesn't run the live tests, but checks that they build, are formatted and have no warnings. Plain `cargo test` only runs spr's own tests.

The live and manual tests don't run in CI: they need a token and a test repository, and the manual ones a person. Run the live tests when you change how spr works with GitHub, and before a release; run `manual init` when you change `spr init` or authentication.

The pre-commit hooks (`.pre-commit-config.yaml`) run the formatters and clippy locally, before each commit. Building the documentation is a separate workflow (`book.yml`), which publishes it from `master`.

## Writing tests

- **Unit tests:** add them to the `tests` module of the code you change. For anything with commits, use `TestRepo`.
- **Live tests:** each scenario is a function in `livetest/src/scenarios/`, registered in `all()` in `scenarios/mod.rs`. It gets a `Ctx` with the test's clone (`ctx.env`, to run spr and Git), the GitHub API (`ctx.api`) and the run (`ctx.run`), and helpers like `commit`, `amend`, `pr_number`, `check_pr_matches` (the pull request has the local commit's tree), `check` and `check_output`. To simulate somebody else pushing to a pull request, change a file through the API (`ctx.api.put_file`), like editing it in GitHub's web interface. GitHub takes a moment to reflect some changes; wait for them with `wait_for` instead of sleeping.
