# Release Checklist

This is internal documentation, listing the step to make a new release. For a pre-release, see [Pre-releases](#pre-releases) below.

## Release commit

- Update version number in `Cargo.toml`
- Run `cargo check` to propagate change to `Cargo.lock`
- Update `CHANGELOG.md`:
  - Rename the top section from "Unreleased" to "version - date" (see previous releases for how it's supposed to look)
  - Make sure significant changes are listed
  - Add a reference to the release on GitHub at the bottom of the file (like for all the other releases)
- Make a commit with the above changes named "Release x.y.z"
- Push that commit to master
- Tag the commit - as it is on master - "vx.y.z"

## GitHub

- Make a release on GitHub: https://github.com/spacedentist/spr/releases/new

## crates.io

- Run `cargo publish` (you might need to do `cargo login` first)

## nixpkgs

- Clone/check out current master of https://github.com/NixOS/nixpkgs
- Edit `pkgs/by-name/sp/spr/package.nix`, and update the "version" field. Also, make a random change to the `hash` and `cargoHash` fields, to make sure the following nix build will not used an existing build.
- Run `nix-build -A spr`
- There will be a hash mismatch error. Edit the nix file again and paste in the correct hash from the build error.
- Run `nix-build -A spr` again
- There will be another hash error, this time in the `cargoHash` field. Again, edit the nix file and paste the correct hash as displayed in the build error.
- Run `nix-build -A spr` again
- If there are any more build errors, fix them and build again.
- Once the build completes without errors, continue with the below.
- Make a git commit with the change in the nix file. Commit message: "spr: old-version -> new-version", e.g. "spr 1.3.2 -> 1.3.3".
- Push the commit to GitHub (probably as the master branch of a nixpkgs fork) and submit a pull request to upstream. Example: https://github.com/NixOS/nixpkgs/pull/179332
- Check in with the pull request and make sure it gets merged.

## homebrew

- Example PR: https://github.com/Homebrew/homebrew-core/pull/221792
- Typically, only `url` and `sha256` need to be updated in `Formula/s/spr.rb` - the BrewTestBot will automatically add a commit to the PR updating the "bottle" section

## Start next release cycle

- Bump the version number in `Cargo.toml` and add `-beta.1` suffix
- Run `cargo check` to propagate change to `Cargo.lock`
- Add a new "Unreleased" section at the top of `CHANGELOG.md`
- Make a commit with the above changes named "Start next development cycle"
- Push that commit to master - the last release commit should be the direct parent

## Pre-releases

A pre-release lets users try what's on master before the release, e.g. 2.0.0-beta.1 before 2.0.0. Its version is the one master has during the release cycle, `x.y.z-beta.N`, and the next pre-release counts `N` up. Only GitHub and crates.io get pre-releases: users of `brew install spr`, nixpkgs and `cargo install spr` keep getting the latest stable release.

- Release commit as above, keeping the version as it is (e.g. `2.0.0-beta.1`): in `CHANGELOG.md`, rename "Unreleased" to "2.0.0-beta.1 - date" and add the link at the bottom; commit "Release 2.0.0-beta.1"; push it to master; tag it "v2.0.0-beta.1".
- GitHub: make a release from the tag, with **"Set as a pre-release"** ticked, so that the latest stable release stays the "Latest" one. Release notes: the version's section of `CHANGELOG.md`. With the GitHub CLI: `gh release create v2.0.0-beta.1 --prerelease --title 2.0.0-beta.1 --notes-file NOTES.md`
- crates.io: `cargo publish`. Cargo treats versions with a suffix as pre-releases: `cargo install spr` keeps installing the latest stable release, `cargo install spr --version 2.0.0-beta.1` installs the pre-release.
- Skip nixpkgs and Homebrew: nixpkgs packages stable releases, and homebrew-core only accepts stable versions. Nix users can run a pre-release from the flake: `nix run github:spacedentist/spr/v2.0.0-beta.1`.
- Start the next cycle as below, but with the next pre-release version (e.g. `2.0.0-beta.2`).

For the final release after pre-releases (e.g. 2.0.0): `CHANGELOG.md` keeps the pre-releases' sections as they are, and the release's section lists the changes since the last pre-release.
