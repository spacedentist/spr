# Changelog

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
