{
  cairo,
  callPackage,
  curl,
  gh,
  glib,
  lib,
  librsvg,
  libxkbcommon,
  makeWrapper,
  pango,
  pipewire,
  pulseaudio,
  python3,
  wireplumber,
  wl-clipboard,
  wtype,
  pkg-config,
  rustPlatform,
  writeShellScriptBin,
  xdg-utils,
}: let
  player = callPackage ./ytmd.nix {};
  python = python3.withPackages (ps: [ps.ytmusicapi]);
  library = writeShellScriptBin "lintel-ytmusic" ''
    exec ${python}/bin/python ${../music.py} "$@"
  '';
in
  rustPlatform.buildRustPackage {
    pname = "lintel";
    version = "0.1.0";
    src = lib.cleanSource ../.;
    cargoLock.lockFile = ../Cargo.lock;
    nativeBuildInputs = [makeWrapper pkg-config];
    buildInputs = [cairo glib librsvg pango libxkbcommon];
    postFixup = ''
      wrapProgram "$out/bin/lintel" --prefix PATH : "${lib.makeBinPath [curl gh library player pipewire pulseaudio wireplumber wl-clipboard wtype xdg-utils]}"
    '';
    meta.mainProgram = "lintel";
  }
