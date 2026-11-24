# Lintel

A native Wayland shell for Niri with an edge-attached bar, notification history
and a tray.

Run it in a Wayland session with an available notification and tray bus name.

```sh
nix run .
nix run . -- player
```

The player command opens the terminal playback client.

Hover at the center of the top edge to reveal the bar. Open the music button for
cover art, controls and playlists. Each playlist starts shuffled. Add
`--playlist NAME=ID` to pin a playlist alongside the signed-in library.

Select **Sign in to YouTube Music** and sign in to Google in the browser window.
Close that window once YouTube Music opens. Lintel then connects your library. Set
`LINTEL_LOGIN_BROWSER` to choose a Chromium-based browser for sign-in.

Lintel reloads your library after sign-in and checks the playing playlist every
15 seconds. Run `lintel music --help` for search, playlist edits, and lyrics.
Playback uses ytmd and depends on whether it can stream each track.

The audio panel controls the default output and input. With the music mic
module enabled, applications use the regular **Microphone**. **Music to mic**
adds or removes the stereo player stream from that input. Set
`services.lintel.musicMic.voiceSource` to the internal filtered voice
node and, if replacing an existing microphone, use its PipeWire name for
`sourceName` so existing application selections carry over.
