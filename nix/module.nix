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
          scale = lib.mkOption {
            type = lib.types.ints.positive;
            default = 1;
            example = 2;
            description = ''
              Output scale clients are told to draw at: at 2 a 4K screen is
              1920x1080 to them, at twice the pixel density. Home, the
              on-screen keyboard and the cursor are unaffected.
            '';
          };
          idle_timeout = lib.mkOption {
            type = lib.types.nullOr lib.types.ints.positive;
            default = null;
            example = 600;
            description = ''
              Seconds without activity before the screen blanks (DPMS off).
              Unset is 10 minutes.
            '';
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

    # In hid-steam's default lizard mode the Steam Controller pretends to be
    # a keyboard and mouse, and its gamepad node stays silent.
    boot.extraModprobeConfig = "options hid_steam lizard_mode=0";

    # Anything opening the Steam Controller puck's hidraw makes hid-steam
    # unregister the gamepad, and Steam's udev rules (60-steam-input) hand
    # that hidraw to the seat's user. Take it back, so nothing the user runs
    # can knock the controller out. This has to sort after the Steam rules and
    # before 73-seat-late, which turns the uaccess tag into an ACL.
    services.udev.packages = [
      (pkgs.writeTextDir "lib/udev/rules.d/61-emrakul-steam-hidraw.rules" ''
        SUBSYSTEM=="hidraw", KERNELS=="*:28DE:*", TAG-="uaccess", MODE="0600", GROUP="root"
      '')
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
