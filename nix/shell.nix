{
  cairo,
  callPackage,
  curl,
  ffmpeg-headless,
  glib,
  lib,
  libxkbcommon,
  makeWrapper,
  pango,
  pipewire,
  pulseaudio,
  python3,
  wireplumber,
  pkg-config,
  rustPlatform,
  writeShellScriptBin,
}: let
  player = callPackage ./ytmd.nix {};
  python = python3.withPackages (ps: [ps.ytmusicapi]);
  library = writeShellScriptBin "lintel-ytmusic" ''
    exec ${python}/bin/python ${../shell/music.py} "$@"
  '';
in
  rustPlatform.buildRustPackage {
    pname = "lintel";
    version = "0.1.0";
    src = lib.cleanSource ../.;
    cargoRoot = "shell";
    buildAndTestSubdir = "shell";
    cargoLock = {
      lockFile = ../shell/Cargo.lock;
      outputHashes."wllib-0.1.0" = "sha256-E6gAnb+I5GQdOpkGlXVIllsnvTbn3eSP4pzVFoDGYHM=";
    };
    nativeBuildInputs = [makeWrapper pkg-config];
    buildInputs = [cairo glib pango libxkbcommon];
    postFixup = ''
      wrapProgram "$out/bin/lintel" --prefix PATH : "${lib.makeBinPath [curl ffmpeg-headless library player pipewire pulseaudio wireplumber]}"
    '';
    meta.mainProgram = "lintel";
  }
