# SPDX-License-Identifier: GPL-3.0-or-later
{
  description = "choom — minimal SSH terminal sharing relay";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    { self, nixpkgs, crane }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      pkgsFor =
        system:
        import nixpkgs {
          inherit system;
          overlays = [ self.overlays.default ];
        };
    in
    {
      # Reusable overlay so other flakes (e.g. the fr0st system flake) get pkgs.choom.
      # choom 2.0 is a Rust workspace; build the `choom` binary with crane.
      overlays.default = final: _prev: {
        choom =
          let
            craneLib = crane.mkLib final;
          in
          craneLib.buildPackage {
            pname = "choom";
            version = "2.0.1";
            # Use the full (git-tracked) flake source, not cleanCargoSource: the build
            # embeds non-Cargo files (the bundled default key, gzipped web assets) via
            # include_bytes!/include_str!, and postInstall needs the man pages.
            src = self;
            strictDeps = true;
            nativeBuildInputs = [ final.installShellFiles ];
            cargoExtraArgs = "--locked -p choom-cli";
            # The integration tests spin up SSH/PTY/sockets; run them via `cargo test`
            # in the devShell, not in the sandboxed package build.
            doCheck = false;
            postInstall = ''
              installManPage man/choom.1 man/choom-server.1 man/choom-host.1 man/choom-clients.1
            '';
            meta = {
              description = "Minimal SSH terminal sharing relay";
              license = final.lib.licenses.gpl3Plus;
              mainProgram = "choom";
            };
          };
      };

      # NixOS module to run the relay as a hardened systemd service. Brings the
      # overlay so `pkgs.choom` is available without extra wiring.
      nixosModules.default = {
        imports = [ ./nix/nixos-module.nix ];
        nixpkgs.overlays = [ self.overlays.default ];
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
              pkgs.cargo
              pkgs.rustc
              pkgs.rustfmt
              pkgs.clippy
              pkgs.rust-analyzer
              pkgs.go-task
            ];
          };
        }
      );

      formatter = forAllSystems (system: (pkgsFor system).nixfmt-rfc-style);
    };
}
