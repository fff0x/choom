# SPDX-License-Identifier: GPL-3.0-or-later
{
  description = "choom — tmate without tmux: minimal SSH terminal sharing relay";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      pkgsFor = system: import nixpkgs {
        inherit system;
        overlays = [ self.overlays.default ];
      };
    in
    {
      # Reusable overlay so other flakes (e.g. the fr0st system flake) get pkgs.choom.
      overlays.default = final: _prev: {
        choom = final.python3Packages.callPackage ./nix/package.nix { src = self; };
      };

      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          inherit (pkgs) choom;
          default = pkgs.choom;
        }
      );

      apps = forAllSystems (system: rec {
        choom = {
          type = "app";
          program = "${(pkgsFor system).choom}/bin/choom";
        };
        default = choom;
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          default = pkgs.mkShell {
            inputsFrom = [ pkgs.choom ];
            packages = [
              pkgs.python3Packages.pytest
              pkgs.python3Packages.pip
            ];
          };
        }
      );

      formatter = forAllSystems (system: (pkgsFor system).nixfmt-rfc-style);
    };
}
