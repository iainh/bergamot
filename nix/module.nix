{ self }:
{ config, lib, pkgs, ... }:

let
  cfg = config.services.bergamot;

  toStr = v:
    if builtins.isBool v then (if v then "yes" else "no")
    else toString v;

  settingsArgs = lib.mapAttrsToList
    (name: value: "-o ${name}=${lib.escapeShellArg (toStr value)}")
    cfg.settings;

  runtimeDeps = with pkgs; [
    unrar-free
    p7zip
    python3
  ];
in
{
  options.services.bergamot = {
    enable = lib.mkEnableOption "bergamot, an efficient Usenet binary downloader";

    package = lib.mkPackageOption pkgs "bergamot" {
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "bergamot";
      description = "User account under which bergamot runs.";
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "bergamot";
      description = "Group under which bergamot runs.";
    };

    dataDir = lib.mkOption {
      type = lib.types.path;
      default = "/var/lib/bergamot";
      description = "Directory for bergamot state, config, and queue data.";
    };

    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Whether to open the web UI port in the firewall.";
    };

    settings = lib.mkOption {
      type = lib.types.attrsOf (lib.types.oneOf [ lib.types.str lib.types.int lib.types.bool ]);
      default = { };
      example = lib.literalExpression ''
        {
          MainDir = "/data/usenet";
          DestDir = "/data/usenet/completed";
          Server1.Host = "news.example.com";
          Server1.Port = 563;
          Server1.Encryption = true;
          Server1.Connections = 8;
        }
      '';
      description = ''
        bergamot configuration options passed as `-o key=value` on the command line.
        Boolean values are converted to "yes"/"no". See
        [docs/08-configuration.md](https://github.com/iainh/bergamot/blob/main/docs/08-configuration.md)
        for the full list of options.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    services.bergamot.settings = {
      MainDir = lib.mkDefault cfg.dataDir;
      ControlIP = lib.mkDefault "0.0.0.0";
      ControlPort = lib.mkDefault 6789;
    };

    systemd.services.bergamot = {
      description = "bergamot Usenet downloader";
      after = [ "network.target" ];
      wantedBy = [ "multi-user.target" ];

      path = runtimeDeps;

      preStart = ''
        if [ ! -f ${cfg.dataDir}/bergamot.conf ]; then
          install -m 0600 ${cfg.package}/share/bergamot/bergamot.conf.sample ${cfg.dataDir}/bergamot.conf
        fi
      '';

      serviceConfig = {
        Type = "simple";
        User = cfg.user;
        Group = cfg.group;
        UMask = "0002";
        StateDirectory = lib.mkIf (cfg.dataDir == "/var/lib/bergamot") "bergamot";
        StateDirectoryMode = "0750";
        ExecStart = lib.concatStringsSep " " ([
          "${cfg.package}/bin/bergamot"
          "--foreground"
          "--config ${cfg.dataDir}/bergamot.conf"
        ] ++ settingsArgs);
        Restart = "on-failure";
        RestartSec = 5;

        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectControlGroups = true;
        ProtectKernelModules = true;
        ProtectKernelTunables = true;
        ReadWritePaths = [
          cfg.dataDir
          cfg.settings.DestDir or ""
          cfg.settings.InterDir or ""
        ];
      };
    };

    networking.firewall.allowedTCPPorts = lib.mkIf cfg.openFirewall [
      cfg.settings.ControlPort
    ];

    users.users = lib.mkIf (cfg.user == "bergamot") {
      bergamot = {
        isSystemUser = true;
        group = cfg.group;
        home = cfg.dataDir;
        description = "bergamot service user";
      };
    };

    users.groups = lib.mkIf (cfg.group == "bergamot") {
      bergamot = { };
    };
  };
}
