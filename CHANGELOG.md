# Changelog

## [Unreleased]

### Fixes

- register base branch at PR creation time instead of after

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
