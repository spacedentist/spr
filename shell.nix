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
    pkg-config
    openssl
  ];
}
