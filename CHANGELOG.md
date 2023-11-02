# Changelog

## Unreleased

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

[1.0.0]: https://github.com/getcord/spr/releases/tag/v1.0.0
[1.1.0]: https://github.com/getcord/spr/releases/tag/v1.1.0
[1.2.0]: https://github.com/getcord/spr/releases/tag/v1.2.0
[1.2.1]: https://github.com/getcord/spr/releases/tag/v1.2.1
[1.2.2]: https://github.com/getcord/spr/releases/tag/v1.2.2
[1.2.3]: https://github.com/getcord/spr/releases/tag/v1.2.3
[1.2.4]: https://github.com/getcord/spr/releases/tag/v1.2.4
[1.3.0]: https://github.com/getcord/spr/releases/tag/v1.3.0
[1.3.1]: https://github.com/getcord/spr/releases/tag/v1.3.1
[1.3.2]: https://github.com/getcord/spr/releases/tag/v1.3.2
[1.3.3]: https://github.com/getcord/spr/releases/tag/v1.3.3
[1.3.4]: https://github.com/getcord/spr/releases/tag/v1.3.4
[1.3.5]: https://github.com/getcord/spr/releases/tag/v1.3.5
