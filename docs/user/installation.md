# Installation

## Binary Installation

### Using Homebrew

```shell
brew install spr
```

### Using Nix

spr is available in nixpkgs

```shell
nix run nixpkgs#spr
```

### Using Cargo

If you have Cargo installed (the Rust build tool), you can install spr by running `cargo install spr`.

## Install from Source

spr is written in Rust. You need a Rust toolchain to build from source. See [rustup.rs](https://rustup.rs) for information on how to install Rust if you have not got a Rust toolchain on your system already.

With Rust all set up, clone this repository and run `cargo build --release`. The spr binary will be in the `target/release` directory.
