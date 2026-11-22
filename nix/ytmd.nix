{
  alsa-lib,
  buildGoModule,
  fetchFromGitHub,
  go_1_27,
  lib,
  libpulseaudio,
  makeWrapper,
}:
(buildGoModule.override {go = go_1_27;}) (finalAttrs: {
  pname = "ytmd";
  version = "0.3.0";
  src = fetchFromGitHub {
    owner = "xlanstar";
    repo = "yt-music-player";
    rev = "v${finalAttrs.version}";
    hash = "sha256-4sYShXhEIj6vqzT8SVvBS6tx2ucgSDxihk9dSx+LxCo=";
  };
  patches = [./ytmd-playlist-edit.patch];
  vendorHash = "sha256-q6Yt7gRTlrIV32W5LV6USmrtHauopPgXfUsFw9tzIJk=";
  subPackages = ["cmd/ytm" "cmd/ytmd"];
  env.CGO_ENABLED = 0;
  ldflags = ["-s" "-w" "-X yt-music-player/internal/version.Version=${finalAttrs.version}"];
  nativeBuildInputs = [makeWrapper];
  postFixup = ''
    wrapProgram "$out/bin/ytmd" \
      --prefix LD_LIBRARY_PATH : "${lib.makeLibraryPath [libpulseaudio alsa-lib]}"
  '';
  meta.platforms = ["x86_64-linux"];
})
