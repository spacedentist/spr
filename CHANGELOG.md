# Changelog

## Unreleased

### Improvements

- spr no longer uses the `git` command line tool: it fetches and pushes via
  libgit2, using the GitHub token, from the repository's GitHub https URL
  (#232)
- support Git configurations that push via ssh, e.g. with `pushInsteadOf`
  (@arichardson, #244)
- commit messages: spr stores its metadata as git trailers (`Pull-request:`,
  `Reviewers:`, `Reviewed-by:`); commit messages written by older versions
  are still recognised, and converted when spr updates them
- remove the "Test Plan" and "Summary" sections: the commit message body is
  the pull request description
- new config option `spr.mergeMethod` (`squash` or `merge`): how `spr land`
  merges pull requests
- new config option `spr.stackingMode`: how pull requests of stacked commits
  are set up — with synthetic base branches (`base-branches`, the default
  with squash-merging), chained on the parent commit's pull request (`chain`,
  the default with merge commits), or chained and linked as a stack on
  GitHub (`github-stack`, opt-in, using GitHub's stacked pull requests
  preview; `spr land` then lands the current commit's pull request together
  with all pull requests below it, #107)
- local commits can have an ID, in a `Spr-Id` trailer (only in local
  commit messages, not in pull requests or landed commits), added by
  `spr diff --spr-id` and `spr patch --spr-id`, or always with the new
  config option `spr.sprIds`; for commits with an ID, spr records the pull
  request head it last pushed in the local ref `refs/spr/<id>/head` (see
  the new page "Identify Commits with Spr-Ids" in the docs)
- for commits with a `Spr-Id`, `spr diff` stops if somebody else pushed
  changes to the pull request that aren't in the local commit, instead of
  silently reverting them; `spr diff --force` overwrites them (#246)
- new command `spr pull`: applies the changes somebody else pushed to the
  pull request to the local commit (for commits with a `Spr-Id`; `--all`
  for all commits of the branch) (#140)
- new config option `spr.useCommitTitleForInitialCommit`: use the commit
  title as the message of the first commit of a new pull request, instead
  of "[𝘀𝗽𝗿] initial version" (@justinbaltazar, #189)
- `spr diff` changes a pull request that uses a synthetic base branch to
  target the master branch once the commit is directly based on it (e.g.
  after the pull request below was merged), or `--cherry-pick` is used, and
  deletes the base branch (#251, #249)
- `spr land` and `spr close` change the base of pull requests stacked on the
  landed/closed pull request, instead of leaving them to be closed by GitHub
- `spr diff --dry-run` shows what `spr diff` would do, without changing
  anything (#111)
- new experimental `spr plumbing` commands for use in scripts, which work
  on Git objects only and don't need spr to be configured:
  - `spr plumbing commit-pr` creates the commits that make a pull request
    reflect a local commit; with `--expected-head`, it detects changes
    pushed to the pull request by others (like `git push
--force-with-lease`)
  - `spr plumbing stack` lists the local commits between the target and a
    commit, with their `Pull-request` trailers
  - `spr plumbing land-check` checks that merging a pull request gives the
    same result as applying the local commits (the check `spr land` does)
- logging, enabled via `RUST_LOG` (#236), and better error reporting (#242,
  #243)
- documentation: new pages on merge methods and stacking modes, on how
  stacked pull requests work, and on the plumbing commands

### Fixes

- `spr diff --all` no longer loses the links to newly created pull requests
  if updating a later commit's pull request fails (the next run created
  them again)
- `spr diff` no longer drops commits made while it's running (e.g. while
  waiting for an update message)
- `spr amend` keeps trailers that only exist locally, e.g. `Signed-off-by`
- `spr amend` no longer rewrites commits just because GitHub returned the
  reviewers in a different order
- when landing fails, `spr land` reliably changes the pull request's base
  back to its base branch (#175)
- `spr land` no longer fails if GitHub has already deleted the landed
  pull request's branch
- `spr patch --branch-name` no longer overwrites an existing branch
- errors looking up the reviewers of a new pull request are reported as
  they are, rather than as "unknown user" (#147)
- the GitHub auth token no longer appears in debug logs
- spr no longer panics on pull request numbers that are too large, or if
  the Git hooks configuration can't be read
- missing configuration is reported with a hint to run `spr init`
- fix a panic when ssh credentials are requested without a username
- the documentation website is deployed again (it failed with mdbook 0.5)

### Other

- update dependencies (git2 0.21, octocrab 0.54, and others), move to the
  2024 Rust edition
- development: Nix shell with all tools, pre-commit hooks for formatting and
  clippy, CI also checks formatting

## [1.3.7] - 2025-08-25

### Improvements

- use GitHub device auth flow for obtaining token
- include all source errors in generic error handling (@quodlibetor)

### Fixes

- fix octocrab routes (include leading slash) (@yamadapc)
- fix clippy warnings

## [1.3.6] - 2025-04-27

### Fixes

- fix regex to recognise GitHub owner and repo names that contain a period character (`.`) (@sqwxl)
- move ownership of the spr repository in GitHub from getcord (the no longer existing command) to spacedentist (the primary author's personal account)
- fix references to getcord (@AlbertQM)
- fix build problems with Rust 1.80 (@chenrui333)
- command line option `--branch-prefix` was ignored (@davinkevin)
- add `--github-master-branch` command line option (@davinkevin)
- update dependencies, fix warnings

### Improvements

- use opentls instead of rustls in order to use system CAs, update octocrab to 0.38 for that (@mayanez)
- add more documentation: how it works for simple PRs (@joneshf)
- make GraphQL queries through octocrab instead of reqwest (@jtietema)
- add `--refs` option to spr diff (@DylanZA)

## [1.3.5] - 2023-11-02

### Fixes

- don't line-wrap URLs (@keyz)
- fix base branch name for github protected branches (@rockwotj)
- fix clippy warnings (@spacedentist)

### Improvements

- turn repository into Cargo workspace (@spacedentist)
- documentation improvements (@spacedentist)
- add shorthand for `--all` (@rockwotj)
- don't fetch all users/teams to check reviewers (@andrewhamon)
- add refname checking (@cadolphs)
- run post-rewrite hooks (@jwatzman)

## [1.3.4] - 2022-07-18

### Improvements

- add config option to make test plan optional (@orausch)
- add comprehensive documentation (@oyamauchi)
- add a `close` command (@joneshf)
- allow `spr format` to be used without GitHub credentials
- don't fail on requesting reviewers (@joneshf)

## [1.3.3] - 2022-06-27

### Fixes

- get rid of italics in generated commit messages - they're silly
- fix unneccessary creation of base branches when updating PRs
- when updating an existing PR, merge in master commit if the commit was rebased even if the base tree did not change
- add a final rebase commit to the PR branch when landing and it is necessary to do so to not have changes in the base of this commit, that since have landed on master, displayed as part of this PR

### Improvemets

- add spr version number in PR commit messages
- add `--all` option to `spr diff` for operating on a stack of commits
- updated Rust dependencies

## [1.3.2] - 2022-06-16

### Fixes

- fix list of required GitHub permissions in `spr init` message
- fix aborting Pull Request update by entering empty message on prompt
- fix a problem where occasionally `spr diff` would fail because it could not push the base branch to GitHub

### Improvements

- add `spr.requireApprovals` config field to control if spr enforces that only accepted PRs can be landed
- the spr binary no longer depends on openssl
- add documentation to the docs/ folder
- `spr diff` now warns the user if the local commit message differs from the one on GitHub when updating an existing Pull Request

## [1.3.1] - 2022-06-10

### Fixes

- register base branch at PR creation time instead of after
- fix `--update-message` option of `spr diff` when invoked without making changes to the commit tree

### Security

- remove dependency on `failure` to fix CVE-2019-25010

## [1.3.0] - 2022-06-01

### Improvements

- make land command reject local changes on land
- replace `--base` option with `--cherry-pick` in `spr diff`
- add `--cherry-pick` option to `spr land`

## [1.2.4] - 2022-05-26

### Fixes

- fix working with repositories not owned by an organization but by a user

## [1.2.3] - 2022-05-24

### Fixes

- fix building with homebrew-installed Rust (currently 1.59)

## [1.2.2] - 2022-05-23

### Fixes

- fix clippy warnings

### Improvements

- clean-up `Cargo.toml` and update dependencies
- add to `README.md`

## [1.2.1] - 2022-04-21

### Fixes

- fix calculating base of PR for the `spr patch` command

## [1.2.0] - 2022-04-21

### Improvements

- remove `--stack` option: spr now bases a diff on master if possible, or otherwise constructs a separate branch for the base of the diff. (This can be forced with `--base`.)
- add new command `spr patch` to locally check out a Pull Request from GitHub

## [1.1.0] - 2022-03-18

### Fixes

- set timestamps of PR commits to time of submitting, not the time the local commit was originally authored/committed

### Improvements

- add `spr list` command, which lists the user's Pull Requests with their status
- use `--no-verify` option for all git pushes

## [1.0.0] - 2022-02-10

### Added

- Initial release

[1.0.0]: https://github.com/spacedentist/spr/releases/tag/v1.0.0
[1.1.0]: https://github.com/spacedentist/spr/releases/tag/v1.1.0
[1.2.0]: https://github.com/spacedentist/spr/releases/tag/v1.2.0
[1.2.1]: https://github.com/spacedentist/spr/releases/tag/v1.2.1
[1.2.2]: https://github.com/spacedentist/spr/releases/tag/v1.2.2
[1.2.3]: https://github.com/spacedentist/spr/releases/tag/v1.2.3
[1.2.4]: https://github.com/spacedentist/spr/releases/tag/v1.2.4
[1.3.0]: https://github.com/spacedentist/spr/releases/tag/v1.3.0
[1.3.1]: https://github.com/spacedentist/spr/releases/tag/v1.3.1
[1.3.2]: https://github.com/spacedentist/spr/releases/tag/v1.3.2
[1.3.3]: https://github.com/spacedentist/spr/releases/tag/v1.3.3
[1.3.4]: https://github.com/spacedentist/spr/releases/tag/v1.3.4
[1.3.5]: https://github.com/spacedentist/spr/releases/tag/v1.3.5
[1.3.6]: https://github.com/spacedentist/spr/releases/tag/v1.3.6
[1.3.7]: https://github.com/spacedentist/spr/releases/tag/v1.3.7
