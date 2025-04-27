# Release Checklist

This is internal documentation, listing the step to make a new release.

## Release commit

* Update version number in `spr/Cargo.toml`
* Run `cargo check` to propagate change to `Cargo.lock`
* Update `CHANGELOG.md`:
  * Rename the top section from "Unreleased" to "version - date" (see previous releases for how it's supposed to look)
  * Make sure significant changes are listed
  * Add a reference to the release on GitHub at the bottom of the file (like for all the other releases)
* Make a commit with the above changes named "Release x.y.z"
* Push that commit to master
* Tag the commit - as it is on master - "vx.y.z"

## GitHub

* Make a release on GitHub: https://github.com/spacedentist/spr/releases/new

## crates.io

* Run `cargo publish -p spr` (you might need to do `cargo login` first)

## nixpkgs

* Clone/check out current master of https://github.com/NixOS/nixpkgs
* Edit `pkgs/development/tools/spr/default.nix`, and update the "version" field. Also, make a random change to the `hash` and `cargoHash` fields, to make sure the following nix build will not used an existing build.
* Run `nix-build -A spr`
* There will be a hash mismatch error. Edit the nix file again and paste in the correct hash from the build error.
* Run `nix-build -A spr` again
* There will be another hash error, this time in the `cargoHash` field. Again, edit the nix file and paste the correct hash as displayed in the build error.
* Run `nix-build -A spr` again
* If there are any more build errors, fix them and build again.
* Once the build completes without errors, continue with the below.
* Make a git commit with the change in the nix file. Commit message: "spr: old-version -> new-version", e.g. "spr 1.3.2 -> 1.3.3".
* Push the commit to GitHub (probably as the master branch of a nixpkgs fork) and submit a pull request to upstream. Example: https://github.com/NixOS/nixpkgs/pull/179332
* Check in with the pull request and make sure it gets merged.

## homebrew

* Example PR: https://github.com/Homebrew/homebrew-core/pull/221792
* Typically, only `url` and `sha256` need to be updated in `Formula/s/spr.rb` - the BrewTestBot will automatically add a commit to the PR updating the "bottle" section

## Start next release cycle

* Bump the version number in `spr/Cargo.toml` and add `-beta.1` suffix
* Run `cargo check` to propagate change to `Cargo.lock`
* Add a new "Unreleased" section at the top of `CHANGELOG.md`
* Make a commit with the above changes named "Start next development cycle"
* Push that commit to master - the last release commit should be the direct parent

