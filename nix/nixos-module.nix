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
        no-default-key = true;
      };
      description = ''
        choom server settings, written to a YAML config (`choom server -c`).
        Keys are the long option names; see `docs/config.yaml`. Paths default to
        the service state directory (/var/lib/choom).
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
    # Sensible state-directory defaults the user can still override via `settings`.
    services.choom-relay.settings = lib.mkMerge [
      {
        host-key = lib.mkDefault "/var/lib/choom/host_key";
        authorized-keys = lib.mkDefault "/var/lib/choom/authorized_keys";
        downloads = lib.mkDefault "/var/lib/choom/downloads";
      }
    ];

    networking.firewall = lib.mkIf cfg.openFirewall (
      let
        sshPort = cfg.settings.listen-port or 2200;
        webSpec = cfg.settings.web or "";
        webPort =
          let
            m = builtins.match ".*:([0-9]+)" webSpec;
          in
          if m == null then null else lib.toInt (builtins.head m);
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
        MemoryDenyWriteExecute = false; # cpython JIT-free, but keep off to be safe
        SystemCallFilter = [ "@system-service" ];
        CapabilityBoundingSet = "";
      };
    };
  };
}
