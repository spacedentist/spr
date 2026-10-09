# The latest version of spr as a Nix package, e.g.
# ```
# nix run github:spacedentist/spr -- --help
# ```
#
# For development, use `shell.nix` (see README.md).

{
  description = "Submit pull requests for individual, amendable, rebaseable commits to GitHub";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      forAllSystems = nixpkgs.lib.genAttrs nixpkgs.lib.systems.flakeExposed;
      cargoToml = nixpkgs.lib.importTOML ./Cargo.toml;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          spr = pkgs.rustPlatform.buildRustPackage {
            pname = "spr";
            inherit (cargoToml.package) version;

            src = nixpkgs.lib.fileset.toSource {
              root = ./.;
              fileset = nixpkgs.lib.fileset.unions [
                ./Cargo.toml
                ./Cargo.lock
                ./src
                ./tests
                # Cargo needs the manifests of all workspace members (the
                # package only builds spr, though)
                ./livetest
              ];
            };

            cargoLock.lockFile = ./Cargo.lock;

            nativeBuildInputs = [ pkgs.pkg-config ];
            buildInputs = [ pkgs.openssl ];

            meta = {
              inherit (cargoToml.package) description homepage;
              license = nixpkgs.lib.licenses.mit;
              mainProgram = "spr";
            };
          };

          default = self.packages.${system}.spr;
        }
      );
    };
}
