self: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.emrakul;
  toml = pkgs.formats.toml {};
  tty = "tty${toString cfg.vt}";
in {
  options.services.emrakul = {
    enable = lib.mkEnableOption "emrakul, the TV appliance compositor, as the machine's only session";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "emrakul.packages.\${system}.default";
    };

    user = lib.mkOption {
      type = lib.types.str;
      description = "User the compositor and everything it launches runs as.";
    };

    vt = lib.mkOption {
      type = lib.types.ints.positive;
      default = 1;
      description = "Virtual terminal the session takes over. Its getty is switched off.";
    };

    settings = lib.mkOption {
      description = "Written verbatim to emrakul's config.toml.";
      type = lib.types.submodule {
        freeformType = toml.type;
        options = {
          device = lib.mkOption {
            type = lib.types.str;
            example = "/dev/dri/by-path/pci-0000:01:00.0-card";
            description = ''
              DRM card to drive. A by-path name, because cardN numbering is
              decided by probe order and can swap between boots on hybrid
              graphics laptops.
            '';
          };
          connector = lib.mkOption {
            type = lib.types.str;
            example = "HDMI-A-1";
            description = "Connector the TV is on, named the way the kernel names it.";
          };
          mode = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            example = "3840x2160@60";
            description = "Mode to set. Unset picks the display's preferred mode.";
          };
          launch = lib.mkOption {
            type = lib.types.listOf lib.types.str;
            default = [];
            example = ["foot"];
            description = "Command started once the compositor is up.";
          };
        };
      };
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        # Both want the seat. Whichever starts second gets no DRM master and
        # shows nothing, which on a TV looks exactly like a crash.
        assertion = !config.services.displayManager.enable;
        message = "services.emrakul replaces the display manager; disable services.displayManager.";
      }
    ];

    systemd.services."getty@${tty}".enable = false;
    systemd.services."autovt@${tty}".enable = false;

    systemd.services.emrakul = {
      description = "emrakul TV session";
      wantedBy = ["graphical.target"];
      after = ["systemd-user-sessions.service" "plymouth-quit-wait.service"];
      conflicts = ["getty@${tty}.service"];
      environment.XDG_SESSION_TYPE = "wayland";
      serviceConfig = {
        User = cfg.user;
        # PAM's login stack is what makes logind hand this process a seat0
        # session, and with it the DRM and input devices libseat asks for.
        PAMName = "login";
        TTYPath = "/dev/${tty}";
        StandardInput = "tty";
        StandardOutput = "journal";
        StandardError = "journal";
        TTYReset = true;
        TTYVHangup = true;
        TTYVTDisallocate = true;
        UtmpIdentifier = tty;
        UtmpMode = "user";
        # TOML has no null, so unset optionals are left out rather than written.
        ExecStart = "${lib.getExe cfg.package} --config ${toml.generate "emrakul.toml" (lib.filterAttrs (_: v: v != null) cfg.settings)}";
        Restart = "always";
        RestartSec = 2;
      };
    };

    systemd.defaultUnit = "graphical.target";
  };
}
