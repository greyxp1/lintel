{
  config,
  lib,
  pkgs,
  utils,
  ...
}: let
  cfg = config.services.lintel;
in {
  imports = [./audio.nix];

  options.services.lintel = {
    enable = lib.mkEnableOption "the Wayland desktop shell";
    package = lib.mkPackageOption pkgs "lintel" {
      default = null;
      extraDescription = "The package built by the desktop shell repository.";
    };
    playlists = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      default = {};
      description = "Pinned playlists keyed by name, with YouTube Music playlist IDs as values.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [cfg.package];
    systemd.user.services.lintel = {
      description = "Wayland desktop shell";
      wantedBy = ["graphical-session.target"];
      after = ["graphical-session.target"];
      partOf = ["graphical-session.target"];
      enableDefaultPath = false;
      serviceConfig = {
        ExecStart = utils.escapeSystemdExecArgs (
          [(lib.getExe cfg.package)]
          ++ lib.concatLists (lib.mapAttrsToList (name: id: ["--playlist" "${name}=${id}"]) cfg.playlists)
        );
        Restart = "on-failure";
      };
    };
  };
}
