{
  config,
  lib,
  ...
}: let
  cfg = config.services.lintel.musicMic;
  stereo = ["FL" "FR"];
  loopback = args: {
    name = "libpipewire-module-loopback";
    inherit args;
  };
in {
  options.services.lintel.musicMic = {
    enable = lib.mkEnableOption "a stereo microphone combining voice and desktop shell music";
    sourceName = lib.mkOption {
      type = lib.types.str;
      default = "desktop_shell_mic";
      description = "PipeWire node name exposed to applications as the regular microphone.";
    };
    voiceSource = lib.mkOption {
      type = lib.types.str;
      description = "PipeWire node name of the microphone that feeds the mix.";
    };
  };

  config = lib.mkIf cfg.enable {
    services.pipewire.extraConfig.pipewire."92-desktop-shell-music-mic"."context.modules" = [
      (loopback {
        "audio.position" = stereo;
        "capture.props" = {
          "node.name" = "desktop_shell_mix_sink";
          "node.description" = "Microphone mix";
          "media.class" = "Audio/Sink/Internal";
          "node.virtual" = true;
        };
        "playback.props" = {
          "node.name" = cfg.sourceName;
          "node.description" = "Microphone";
          "media.class" = "Audio/Source";
          "node.virtual" = true;
          "priority.session" = 2000;
        };
      })
      (loopback {
        "audio.position" = stereo;
        "capture.props" = {
          "node.name" = "desktop_shell_voice_capture";
          "target.object" = cfg.voiceSource;
          "node.dont-fallback" = true;
          "node.linger" = true;
          "node.passive" = true;
        };
        "playback.props" = {
          "node.name" = "desktop_shell_voice_playback";
          "target.object" = "desktop_shell_mix_sink";
          "node.dont-fallback" = true;
          "node.linger" = true;
          "node.passive" = true;
        };
      })
    ];
  };
}
