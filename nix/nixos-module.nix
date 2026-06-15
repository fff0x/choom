# SPDX-License-Identifier: GPL-3.0-or-later
# NixOS module to run the choom relay as a hardened systemd service.
#
#   imports = [ choom.nixosModules.default ];
#   services.choom-relay = {
#     enable = true;
#     openFirewall = true;
#     settings = { host = "choom.example.com"; web = "0.0.0.0:8080"; };
#   };
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.choom-relay;
  # choom reads a YAML config; JSON is valid YAML, so render settings as JSON.
  configFile = pkgs.writeText "choom.yaml" (builtins.toJSON cfg.settings);
in
{
  options.services.choom-relay = {
    enable = lib.mkEnableOption "the choom SSH terminal-sharing relay";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.choom;
      defaultText = lib.literalExpression "pkgs.choom";
      description = "The choom package to run.";
    };

    settings = lib.mkOption {
      type = lib.types.attrsOf lib.types.anything;
      default = { };
      example = {
        host = "choom.example.com";
        web = "0.0.0.0:8080";
        use_default_key = false;
        authorized_keys = [ "ssh-ed25519 AAAA... alice" ];
        record_dir = "/var/lib/choom/recordings";
      };
      description = ''
        choom server settings, written to a YAML config (`choom server -c`).
        Keys are the snake_case config keys; see `docs/config.yaml`. Unknown keys
        are rejected by the relay.
      '';
    };

    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Open the configured SSH (and web) ports in the firewall.";
    };

    environmentFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "Optional EnvironmentFile for secrets (passed to the unit).";
    };
  };

  config = lib.mkIf cfg.enable {
    networking.firewall = lib.mkIf cfg.openFirewall (
      let
        portOf =
          spec: default:
          let
            m = builtins.match ".*:([0-9]+)" spec;
          in
          if m == null then default else lib.toInt (builtins.head m);
        sshPort = portOf (cfg.settings.ssh_addr or "0.0.0.0:2200") 2200;
        webSpec = cfg.settings.web or "";
        webPort = if webSpec == "" then null else portOf webSpec 8080;
      in
      {
        allowedTCPPorts = [ sshPort ] ++ lib.optional (webPort != null) webPort;
      }
    );

    systemd.services.choom-relay = {
      description = "choom SSH terminal-sharing relay";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package} server -c ${configFile}";
        Restart = "on-failure";
        RestartSec = 2;
        EnvironmentFile = lib.mkIf (cfg.environmentFile != null) cfg.environmentFile;
        # The shell runs on the host, never here — so lock the service down hard.
        DynamicUser = true;
        StateDirectory = "choom";
        WorkingDirectory = "/var/lib/choom";
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        RestrictNamespaces = true;
        RestrictRealtime = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true; # Rust binary, no JIT — W^X is safe
        SystemCallFilter = [ "@system-service" ];
        CapabilityBoundingSet = "";
      };
    };
  };
}
