# Coding Guidelines

Guidelines for developing in this code base. For an overview of the code base and the project's conventions, see [AGENTS.md](AGENTS.md).

---

## Rust Version and Edition

spr uses the **Rust 2024** edition. The minimum supported Rust version is `rust-version` in `Cargo.toml` (CI checks that spr builds with it). We raise it when spr or a dependency needs a newer version, to any version that is at least about six months old.

---

## Formatting

All code must be formatted with `cargo fmt`. `rustfmt.toml` sets a maximum line width of **80 columns**.

```bash
cargo fmt --all
```

rustfmt doesn't break string literals. Keep long strings within 80 columns too, with `\` line continuations:

```rust
bail!(
    "Stacking mode '{stacking_mode}' cannot be used with merge method \
     '{merge_method}'"
);
```

Other files have formatters too: prettier (Markdown, YAML), taplo (TOML) and nixfmt (Nix). The pre-commit hooks (`.pre-commit-config.yaml`) run all of them, and clippy. Format code in the commit that introduces it, not in separate formatting commits.

---

## Linting

All code must pass clippy without warnings. CI runs:

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

**Tip:** Before manually fixing clippy warnings, try the auto-fix feature:

```bash
cargo clippy --fix --allow-dirty
```

---

## Project Structure

The workspace has two packages:

- **`spr`** (the root): a library (`src/lib.rs`) and the `spr` binary (`src/main.rs`). The library is spr's implementation, not a public API.
- **`spr-livetest`** (`livetest/`): live tests that run the spr binary against a real GitHub repository (see [Testing](docs/dev/testing.md)). It doesn't depend on spr's library: it tests spr from the outside, like a user.

The main modules of `spr`:

| Module                                                | Responsibility                                                                                  |
| ----------------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| `main.rs`                                             | Command line (clap), reading the configuration, picking the forge implementation                |
| `commands/`                                           | One module per subcommand (`diff`, `land`, `init`, …), generic over the forge                   |
| `config.rs`                                           | `Config`: the settings of a run; tokens; deprecated options                                     |
| `target.rs`                                           | Targets (`github:owner/repo:branch`): parsing and formatting                                    |
| `forge.rs`                                            | The `Forge` trait: what spr needs from a forge (pull requests, reviews, merging), and its types |
| `github.rs`, `gql/`                                   | The `Forge` implementation for GitHub (REST via octocrab, GraphQL queries and schema in `gql/`) |
| `git.rs`, `git_remote.rs`                             | The local repository (libgit2), and fetching and pushing                                        |
| `message.rs`                                          | Commit messages: parsing, trailers, the pull request description                                |
| `pr_commits.rs`, `land_check.rs`, `remote_changes.rs` | Constructing pull request commits, checking a merge, working out others' changes                |
| `output.rs`                                           | Messages to the user                                                                            |

Forge-specific code belongs in the forge implementation (`github.rs`). Commands work with any `Forge`.

---

## Separating Logic and Side Effects

**This is a core architectural principle.** Functions should be either pure logic or side-effectful — not both.

- **Logic functions** are pure: they take inputs and return outputs. No network requests, no changes to the repository, no output. These are easy to test, easy to reason about, and easy to reuse.
- **Side-effect functions** orchestrate: they gather inputs (from Git, the forge, the configuration), call logic functions, and apply the results. Keep them thin.

```rust
// Good: pure logic — easy to test
fn parse_pull_request_field(repository: &Repository, text: &str) -> Option<u64> {
    // ... pure computation ...
}

// Good: side effects — thin, calls the logic
async fn update_commit(gh: &impl Forge, commit: &mut PreparedCommit) -> Result<()> {
    let number = parse_pull_request_field(gh.config().target.repository(), &trailer);
    let pull_request = gh.get_pull_request(number).await?;
    // ...
}
```

Working on Git objects is a middle ground: building commits and trees in the local object database (e.g. `pr_commits.rs`, the plumbing commands) changes nothing the user sees until a ref is updated or something is pushed, and is easy to test with a temporary repository.

---

## Error Handling

- Use `color_eyre::eyre` for errors: `Result`, `bail!`, `eyre!`.
- Provide context when propagating errors, where it helps the user (`.wrap_err("…")`, `.wrap_err_with(|| format!("…"))`).
- Error messages are for users: say what went wrong and, where possible, what to do about it (e.g. "Run `spr init` to set up spr in this repository.").
- Report problems instead of silently ignoring or "fixing" them (e.g. a `Pull-request` trailer that refers to another repository is an error, not a reason to create a new pull request).

### `.unwrap()` vs `.expect()`

- Avoid bare `.unwrap()` in production code. Use `.expect("reason")` instead, describing the invariant that guarantees the value is present.
- `.unwrap()` is acceptable in tests.

---

## Secrets

Tokens must never end up in output, logs or error messages:

- Types that hold a token implement `Debug` by hand, redacting it (see `Config`, `AuthTokens`, `Cli`).
- Don't put tokens on command lines (the process list shows them to others).

---

## Async Code

- spr runs on a single-threaded tokio runtime (`current_thread`, in a `LocalSet`).
- Prefer `async fn` over manual `Future` implementations; the `Forge` trait uses `async fn` in traits.

---

## Data Serialization

- Use `serde` for the data exchanged with forges' APIs; derive `Serialize`/`Deserialize` where possible.
- GraphQL queries are `.graphql` files in `src/gql/`, checked against GitHub's schema there (`graphql_client`).

---

## User-Facing Output

- Print messages with `output::output(icon, text)`: an emoji, then the text, wrapped to the terminal's width. Write full sentences.
- Commit messages and pull request descriptions are the user's: spr only adds or updates its own trailers, and keeps everything else as it is.

---

## Testing

### Philosophy

Unit test coverage should be **pragmatic**:

- **Do test** logic functions: these are pure, deterministic, and the main place where correctness matters. Verify that the function implements the correct behaviour for meaningful inputs, edge cases, and error conditions.
- **Don't test** trivial code: if a function is a one-liner, a simple delegation, or a struct constructor, a test adds noise without value.
- **Don't test** implementation details: tests should assert on observable behaviour (inputs → outputs), not on internal data structures, call counts, or ordering of internal steps. If a refactor doesn't change behaviour, tests shouldn't break.
- **Do test** boundary conditions and error paths in logic functions.

```rust
// Good test: verifies behaviour
#[test]
fn test_parse_pull_request_field_of_other_repository() {
    let repository = "github:acme/codez".parse().unwrap();
    assert_eq!(
        parse_pull_request_field(&repository, "https://github.com/acme/other/pull/12"),
        None
    );
}

// Bad test: trivially restates the code
#[test]
fn test_config_has_branch_prefix() {
    let config = Config::new(target, "spr/foo/".into(), /* … */).unwrap();
    assert_eq!(config.branch_prefix, "spr/foo/");
}
```

### Conventions

- Unit tests live in a `mod tests` at the end of the module. Use `#[test]` for sync tests, `#[tokio::test]` for async ones.
- Tests that need commits build them in a temporary repository with `TestRepo` (`src/test_utils.rs`).
- Tests never talk to a real forge. To test API requests (paths, headers), use a mock server (`wiremock`, see the tests in `github.rs`).
- Tests that run the spr binary go in `tests/`.
- Behaviour against a real GitHub repository is covered by the live tests (`livetest/`): add or extend a scenario there when you change how spr works with GitHub.
- Tests must be fast and deterministic.
- Use descriptive test names that explain the scenario.

---

## Documentation

### Doc Comments

- Document public items with `///` doc comments.
- Focus doc comments on **what** and **why**, not **how** (the code shows how).

### Inline Comments

Default to no comments — well-named identifiers should explain WHAT the code does. Add a comment only when the WHY is non-obvious. Specifically, always comment:

- **Ordering invariants** — when steps must happen in a specific sequence and reordering would cause subtle bugs (e.g. retargeting the pull requests based on a branch before deleting it).
- **Workarounds for forge behaviour** — when code exists because of how a forge or a library behaves (e.g. replacing octocrab's API version header for the stacks API, or GitHub's delay in updating a pull request after a push).
- **Intentional omissions** — when code deliberately does NOT do something that a reader might expect (e.g. not force-pushing a pull request branch).

These invariants are the easiest things to regress on during refactors. A comment that says "without this, X happens" is worth its weight.

### Module Documentation

A module should have a `//!` comment at the top outlining what it is responsible for, and its key types, unless that's obvious from its name.

### User Documentation

- The user documentation is an mdBook in `docs/` (`docs/SUMMARY.md`). Update it with user-facing changes.
- Configuration options go into `docs/reference/configuration.md`.
- User-facing changes get an entry under "Unreleased" in `CHANGELOG.md`.

---

## Additional Guidelines

### Naming

- Use descriptive names. Avoid single-letter variables except in closures, iterators, or very short scopes.
- Types: `PascalCase`. Functions/methods: `snake_case`. Constants: `SCREAMING_SNAKE_CASE`.
- Prefer domain-specific names over generic ones (`pull_request_number` over `number`, `target_branch` over `branch`), and forge-neutral ones outside the forge implementations (`Branch`, not `GitHubBranch`).

### Dependencies

- Prefer well-maintained, widely-used crates.
- Specify dependencies with their full version (`"^1.2.3"`) in `Cargo.toml`.
- Justify new dependencies — don't add a crate for something the standard library handles. Dependencies needed only by the live tests go into `livetest/Cargo.toml`, test-only ones into `[dev-dependencies]`.
