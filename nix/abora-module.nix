# NixOS module for `aborad`, the Abora Framework system daemon.
#
# The daemon is loopback-only and read-only, so this module:
#   * installs the aborad package and a systemd unit with the same
#     hardening as `installer/systemd/aborad.service`,
#   * writes `/etc/abora/abora.toml` from `services.aborad.settings`,
#   * provisions the bearer-token store at `/var/lib/abora/auth.toml`
#     (mode 0600, SHA-256 hashes only) from `services.aborad.authTokens`,
#     or points `[security].token_file` at `authTokenFile` for secret
#     managers (sops-nix, agenix, ...).
#
# See docs/nix.md for usage.
{ config, lib, pkgs, ... }:

let
  cfg = config.services.aborad;
  aboradPackage = pkgs.callPackage ./aborad-package.nix { };
  settingsFormat = pkgs.formats.toml { };

  tokenFilePath = "/var/lib/abora/auth.toml";

  hasAuthTokens = cfg.authTokens != [];
  hasAuthTokenFile = cfg.authTokenFile != null;

  tokenToString = token:
    ''
      [[tokens]]
      name = "${token.name}"
      permissions = [${lib.concatMapStringsSep ", " (p: "\"${p}\"") token.permissions}]
      secret_hash = "${token.secretHash}"
    '';

  tokenStore = pkgs.writeText "abora-auth.toml" (
    lib.concatMapStringsSep "\n" tokenToString cfg.authTokens
  );

  finalSettings = lib.recursiveUpdate cfg.settings (
    if hasAuthTokens then
      { security.token_file = tokenFilePath; }
    else if hasAuthTokenFile then
      { security.token_file = cfg.authTokenFile; }
    else
      { }
  );

  configFile = settingsFormat.generate "abora.toml" finalSettings;

  inherit (lib) mkOption types mkEnableOption mkIf concatStringsSep;
in
{
  options.services.aborad = {
    enable = mkEnableOption "the Abora Framework system daemon (aborad)";

    package = mkOption {
      type = types.path;
      default = aboradPackage;
      defaultText = lib.literalExpression "pkgs.callPackage ./nix/aborad-package.nix { }";
      description = "aborad package to run.";
    };

    user = mkOption {
      type = types.str;
      default = "aborad";
      description = "User account under which the daemon runs.";
    };

    group = mkOption {
      type = types.str;
      default = "aborad";
      description = "Group account under which the daemon runs.";
    };

    settings = mkOption {
      type = types.submodule { freeformType = settingsFormat.type; };
      default = { };
      description = ''
        Contents of /etc/abora/abora.toml. Omitted sections fall back to the
        daemon's compiled-in defaults; see config/abora.toml.default and
        docs/configuration.md. `[security].token_file` is managed when
        `authTokens` or `authTokenFile` is set.
      '';
    };

    authTokens = mkOption {
      type = types.listOf (types.submodule {
        options = {
          name = mkOption {
            type = types.str;
            description = "Human-readable label for the token (shows in logs).";
          };
          permissions = mkOption {
            type = types.listOf types.str;
            default = [ "read_all" ];
            description = ''
              Permission ids to grant: read:health, read:version, read:system,
              read:services, read:updates, or read_all (grants everything).
            '';
          };
          secretHash = mkOption {
            type = types.str;
            description = ''
              sha256:<hex> produced by `abora auth generate-token`. Only the
              hash is stored; the plaintext secret is shown once by the CLI
              and must be distributed out-of-band.
            '';
          };
        };
      });
      default = [ ];
      description = ''
        Bearer tokens written to /var/lib/abora/auth.toml (mode 0600, owner
        is the service user). Both authTokens and authTokenFile are
        exclusive; prefer authTokenFile when secrets live in sops-nix or
        agenix.
      '';
    };

    authTokenFile = mkOption {
      type = types.nullOr types.path;
      default = null;
      description = ''
        Existing token-store file to mount into `[security].token_file`
        (e.g. from sops-nix or agenix). Exclusive with `authTokens`.
      '';
    };

    openFirewall = mkOption {
      type = types.bool;
      default = false;
      description = ''Open the firewall port for aborad. The daemon only
        listens on loopback (`127.0.0.1:7360`) in this milestone, so this has
        no effect today — it exists to fail loudly if the intent was
        remote access, which is not supported yet.
      '';
    };
  };

  config = mkIf cfg.enable {
    users.users.${cfg.user} = {
      isSystemUser = true;
      group = cfg.group;
    };
    users.groups.${cfg.group} = { };

    environment.etc."abora/abora.toml" = {
      source = configFile;
      # Note: the config file holds no secrets (only paths/flags), so it is
      # world-readable (default 0444) — `aborad` must be able to read it.
    };

    systemd.services.aborad = {
      description = "Abora Framework system daemon";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];

      serviceConfig = {
        Type = "simple";
        ExecStart = "${cfg.package}/bin/aborad --config /etc/abora/abora.toml";
        Restart = "on-failure";
        RestartSec = "2s";
        User = cfg.user;
        Group = cfg.group;
        RuntimeDirectory = "abora";
        RuntimeDirectoryMode = "0750";
        StateDirectory = "abora";
        StateDirectoryMode = "0750";
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        ReadOnlyPaths = [ "/etc/abora" ];
        ReadWritePaths = [ "/var/lib/abora" ];
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        RestrictSUIDSGID = true;
        RestrictRealtime = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        StandardOutput = "journal";
        StandardError = "journal";
      }
      // lib.optionalAttrs hasAuthTokens {
        # ExecStartPre runs as the service user (User applies to all Exec*),
        # which owns /var/lib/abora via StateDirectory, so `install` can write
        # the token store without any root privileges.
        ExecStartPre = [
          "${pkgs.coreutils}/bin/install -m 0600 ${tokenStore} ${tokenFilePath}"
        ];
      };
    };

    assertions = [
      {
        assertion = !(hasAuthTokens && hasAuthTokenFile);
        message = ''
          services.aborad: set either `authTokens` or `authTokenFile`, not both.
        '';
      }
      {
        assertion = !(hasAuthTokens && (cfg.settings.security.token_file or null) != null);
        message = ''
          services.aborad: `authTokens` manages [security].token_file itself; do
          not also set `settings.security.token_file`.
        '';
      }
      {
        assertion = !cfg.openFirewall;
        message = ''
          services.aborad: openFirewall is meaningless — the daemon only binds
          loopback in this milestone (see docs/security.md).
        '';
      }
    ];
  };
}