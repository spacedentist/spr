# Simple nix shell for developing spr
#
# To load automatically with direnv, do
# ```
# echo "use nix" >.envrc
# direnv allow
# ```

{
  pkgs ? import <nixpkgs> { },
}:
pkgs.mkShell {
  packages = with pkgs; [
    cargo
    clippy
    mdbook
    mdbook-mermaid
    nixfmt
    openssl
    pkg-config
    pre-commit
    prettier
    rustc
    rustfmt
    taplo
  ];
}
