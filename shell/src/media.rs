use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{fs, thread};

use gob::{StreamDeserializer, StreamSerializer};
use image::{ImageReader, Limits};
use serde::{Deserialize, Deserializer, Serialize};
use serde_schema_derive::SchemaSerialize;

type MediaResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Default, Clone, PartialEq)]
pub struct Track {
  pub id: String,
  pub queue: Arc<Vec<String>>,
  pub queue_songs: Arc<Vec<QueueSong>>,
  pub queue_index: usize,
  pub position: f64,
  pub duration: f64,
  pub progressing_since: Option<Instant>,
  pub title: String,
  pub artist: String,
  pub art: String,
  pub playing: bool,
  pub has_track: bool,
  pub at_end: bool,
  pub player_pid: u32,
}

impl Track {
  pub fn has_lyric_metadata(&self) -> bool {
    !self.title.is_empty() && !self.artist.is_empty() && self.duration > 0.0
  }

  pub fn elapsed(&self) -> f64 {
    let delta = self
      .progressing_since
      .map_or(0.0, |since| since.elapsed().as_secs_f64());
    if self.duration > 0.0 {
      (self.position + delta).clamp(0.0, self.duration)
    } else {
      (self.position + delta).max(0.0)
    }
  }
}

#[derive(Clone, PartialEq)]
pub struct QueueSong {
  pub title: String,
  pub artist: String,
  pub seconds: i32,
  pub art: String,
}

#[derive(Clone)]
pub struct Bookmark {
  pub id: String,
  pub order: Arc<Vec<String>>,
  pub position: f64,
}

pub struct Art {
  pub data: Vec<u8>,
  pub width: i32,
  pub height: i32,
}

#[derive(Serialize, SchemaSerialize)]
struct Greeting {
  #[serde(rename = "Hello")]
  hello: Hello,
}

#[derive(Serialize, SchemaSerialize)]
#[serde(rename_all = "PascalCase")]
struct Hello {
  version: &'static str,
  #[serde(rename = "PID")]
  pid: i32,
  proto: u16,
  role: u8,
  subscribe: bool,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct Frame {
  #[serde(default, deserialize_with = "present")]
  hello: Option<Peer>,
  #[serde(default, deserialize_with = "present")]
  event: Option<Event>,
  #[serde(rename = "ID")]
  id: u32,
  #[serde(deserialize_with = "present")]
  ahead: Option<Lists>,
  #[serde(deserialize_with = "present")]
  reply: Option<Reply>,
}

#[derive(Deserialize)]
struct Peer {
  #[serde(rename = "Proto")]
  proto: u16,
  #[serde(rename = "PID")]
  pid: i32,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct Event {
  #[serde(default, deserialize_with = "present")]
  state: Option<Playback>,
  #[serde(default, deserialize_with = "present")]
  queue: Option<Queue>,
  songs: Vec<Song>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct Playback {
  current: String,
  index: i32,
  playing: bool,
  waiting: bool,
  position: f64,
  duration: f64,
  shuffled: bool,
}

#[derive(Deserialize)]
struct Queue {
  #[serde(default, rename = "IDs")]
  ids: Vec<String>,
}

#[derive(Clone, Default, Deserialize, Serialize, SchemaSerialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct Song {
  #[serde(rename = "ID")]
  pub id: String,
  pub title: String,
  pub artist: String,
  pub art: String,
  album: String,
  #[serde(rename = "AlbumID")]
  album_id: String,
  pub seconds: i32,
}

// Gob omits nil pointers and encodes present values as the underlying struct.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
  reader: D,
) -> Result<Option<T>, D::Error> {
  T::deserialize(reader).map(Some)
}

fn greeting(role: u8, subscribe: bool) -> Greeting {
  Greeting {
    hello: Hello {
      version: "0.3.0",
      pid: std::process::id() as i32,
      proto: 2,
      role,
      subscribe,
    },
  }
}

fn peer<R: std::io::Read>(input: &mut StreamDeserializer<R>) -> MediaResult<Peer> {
  input
    .deserialize::<Frame>()?
    .and_then(|frame| frame.hello)
    .filter(|peer| peer.proto == 2)
    .ok_or_else(|| "ytmd protocol 2 is required".into())
}

fn subscribe(socket: &Path, updates: &async_channel::Sender<Track>) -> MediaResult<()> {
  let stream = UnixStream::connect(socket)?;
  stream.set_read_timeout(Some(Duration::from_secs(2)))?;
  stream.set_write_timeout(Some(Duration::from_secs(2)))?;
  StreamSerializer::new_with_write(&stream).serialize(&greeting(2, true))?;
  let mut input = StreamDeserializer::new(&stream);
  let player_pid = u32::try_from(peer(&mut input)?.pid)?;
  stream.set_read_timeout(None)?;
  let mut songs = HashMap::new();
  let mut playback = Playback::default();
  let mut order = Arc::new(Vec::new());
  let mut queue_songs = Arc::new(Vec::new());
  let mut progressing_since = None;
  let mut last = Track::default();
  while let Some(frame) = input.deserialize::<Frame>()? {
    let Some(event) = frame.event else { continue };
    let songs_changed = !event.songs.is_empty() || event.queue.is_some();
    for song in event.songs {
      songs.insert(song.id.clone(), song);
    }
    if let Some(queue) = event.queue {
      let mut keep = HashMap::with_capacity(queue.ids.len());
      for id in &queue.ids {
        if let Some(song) = songs.remove(id) {
          keep.insert(id.clone(), song);
        }
      }
      songs = keep;
      order = Arc::new(queue.ids);
    }
    if songs_changed {
      queue_songs = Arc::new(
        order
          .iter()
          .map(|id| {
            let song = songs.get(id);
            QueueSong {
              title: song
                .filter(|song| !song.title.is_empty())
                .map_or_else(|| "Unknown track".into(), |song| song.title.clone()),
              art: song.map_or_else(String::new, |song| song.art.clone()),
              artist: song.map_or_else(String::new, |song| song.artist.clone()),
              seconds: song.map_or(0, |song| song.seconds),
            }
          })
          .collect(),
      );
    }
    if let Some(state) = event.state {
      progressing_since = (state.playing && !state.waiting).then(Instant::now);
      playback = state;
    }
    let mut track = Track {
      id: playback.current.clone(),
      queue: order.clone(),
      queue_songs: queue_songs.clone(),
      queue_index: usize::try_from(playback.index).unwrap_or_default(),
      position: playback.position,
      duration: playback.duration,
      progressing_since,
      playing: playback.playing,
      player_pid,
      has_track: !playback.current.is_empty(),
      // The catalog duration is rounded until the player opens the stream.
      at_end: !playback.playing
        && playback.duration > 0.0
        && playback.position + 0.5 >= playback.duration,
      ..Track::default()
    };
    if let Some(song) = songs.get(&playback.current) {
      track.title.clone_from(&song.title);
      track.artist.clone_from(&song.artist);
      track.art.clone_from(&song.art);
    }
    if track != last {
      last = track.clone();
      if updates.send_blocking(track).is_err() {
        return Ok(());
      }
    }
  }
  Ok(())
}

pub fn has_saved_player() -> bool {
  gio::glib::user_config_dir()
    .join("yt-music-player/daemon.json")
    .is_file()
}

pub fn subscribe_tracks(socket: std::path::PathBuf) -> async_channel::Receiver<Track> {
  let (updates, receive) = async_channel::bounded(4);
  thread::spawn(move || {
    if has_saved_player() {
      run_control(&["daemon", "start"]);
    }
    loop {
      if let Err(error) = subscribe(&socket, &updates)
        && socket.exists()
      {
        eprintln!("Music connection: {error}");
      }
      if updates.send_blocking(Track::default()).is_err() {
        return;
      }
      thread::sleep(Duration::from_secs(2));
    }
  });
  receive
}

pub fn control(command: &'static str, rewind: bool) {
  thread::spawn(move || {
    if rewind && !run_control(&["seek", "0"]) {
      return;
    }
    run_control(&[if rewind { "play" } else { command }]);
  });
}

pub fn seek(seconds: f64) -> bool {
  run_control(&["seek", &format!("{seconds:.2}")])
}

fn run_control(args: &[&str]) -> bool {
  let mut command = Command::new("ytm");
  if crate::audio::music_mix_enabled() {
    // The Pulse server only starts playback on public sinks. MyGo's ALSA
    // backend can address the private sink through PipeWire's default PCM.
    command
      .env("PULSE_SERVER", "unix:/dev/null")
      .env("PIPEWIRE_NODE", crate::audio::MUSIC_SINK);
  }
  match command.args(args).status() {
    Ok(status) => status.success(),
    Err(error) => {
      eprintln!("Music control: {error}");
      false
    }
  }
}

// Method numbers are stable in ytmd protocol 2.
const STATUS: u16 = 4;
const PLAY_ALL: u16 = 6;
const TOGGLE_SHUFFLE: u16 = 16;
const MOVE: u16 = 19;
const SYNC_PLAYLIST_QUEUE: u16 = 33;

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct Playlist {
  #[serde(rename = "ID")]
  pub id: String,
  pub name: String,
  pub art: String,
  pub partial: bool,
  pub songs: Vec<String>,
}

pub fn save_playlist(playlist: Option<&Playlist>) {
  use gio::glib::{FileSetContentsFlags, file_set_contents_full, user_state_dir};
  let directory = user_state_dir().join("lintel");
  let path = directory.join("playlist.json");
  let result: MediaResult<()> = (|| {
    fs::create_dir_all(directory)?;
    let contents = serde_json::to_vec(&playlist)?;
    file_set_contents_full(path, &contents, FileSetContentsFlags::CONSISTENT, 0o600)?;
    Ok(())
  })();
  if let Err(error) = result {
    eprintln!("Save music playlist: {error}");
  }
}

pub fn restore_playlist(queue: &[String], current: &str) -> Option<Playlist> {
  let path = gio::glib::user_state_dir().join("lintel/playlist.json");
  if let Ok(contents) = fs::read(path)
    && let Ok(playlist) = serde_json::from_slice::<Option<Playlist>>(&contents)
  {
    let playlist = playlist?;
    let songs: HashSet<_> = playlist.songs.iter().map(String::as_str).collect();
    return (!playlist.id.is_empty()
      && songs.contains(current)
      && queue.iter().all(|id| songs.contains(id.as_str())))
    .then_some(playlist);
  }

  // Older Lintel builds saved the playlist catalog but didn't record the selection.
  let ids: HashSet<_> = queue.iter().map(String::as_str).collect();
  let mut matched = None;
  let cache = gio::glib::user_cache_dir().join("lintel/playlists");
  for entry in fs::read_dir(cache).ok()?.flatten() {
    let Ok(contents) = fs::read(entry.path()) else {
      continue;
    };
    let Ok(PlaylistReply {
      mut playlist,
      songs,
    }) = serde_json::from_slice(&contents)
    else {
      continue;
    };
    if playlist.id.is_empty()
      || songs
        .iter()
        .map(|song| song.id.as_str())
        .collect::<HashSet<_>>()
        != ids
    {
      continue;
    }
    if matched.is_some() {
      return None;
    }
    playlist.songs = songs.into_iter().map(|song| song.id).collect();
    matched = Some(playlist);
  }
  matched
}

impl Playlist {
  pub fn pin(value: &str) -> MediaResult<Self> {
    let (name, id) = value.split_once('=').ok_or("expected playlist NAME=ID")?;
    let name = name.trim();
    let id = id.trim();
    if name.is_empty()
      || id.is_empty()
      || id.len() > 128
      || !id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
      return Err("expected a playlist name and a YouTube Music playlist ID".into());
    }
    Ok(Self {
      name: name.to_owned(),
      id: id.to_owned(),
      ..Self::default()
    })
  }
}

#[derive(Default, Serialize, SchemaSerialize)]
#[serde(rename_all = "PascalCase")]
struct Call {
  method: u16,
  #[serde(rename = "Ref")]
  reference: Option<Reference>,
  #[serde(rename = "IDs")]
  ids: Option<QueueSelection>,
  #[serde(rename = "Move")]
  move_args: Option<MoveArgs>,
}

#[derive(Serialize, SchemaSerialize)]
#[serde(rename_all = "PascalCase")]
struct MoveArgs {
  from: i32,
  to: i32,
  expected: Vec<String>,
}

#[derive(Serialize, SchemaSerialize)]
struct Reference {
  #[serde(rename = "ID")]
  id: String,
  #[serde(rename = "SongID")]
  song_id: String,
}

#[derive(Serialize, SchemaSerialize)]
#[serde(rename_all = "PascalCase")]
struct QueueSelection {
  #[serde(rename = "IDs")]
  ids: Vec<String>,
  expected: Vec<String>,
  start: i32,
  ordered: bool,
  position: f64,
}

#[derive(Serialize, SchemaSerialize)]
struct Outgoing {
  #[serde(rename = "ID")]
  id: u32,
  #[serde(rename = "Call")]
  call: Option<Call>,
  #[serde(rename = "Ahead")]
  ahead: Option<SongPart>,
}

#[derive(Serialize, SchemaSerialize)]
struct SongPart {
  #[serde(rename = "Songs")]
  songs: Vec<Song>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct Reply {
  #[serde(deserialize_with = "present")]
  err: Option<PlayerError>,
  #[serde(deserialize_with = "present")]
  status: Option<Status>,
}

#[derive(Deserialize)]
struct PlayerError {
  #[serde(rename = "Code")]
  code: u16,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct Lists {
  songs: Vec<Song>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct PlaylistReply {
  playlist: Playlist,
  songs: Vec<Song>,
}

impl PlaylistReply {
  fn into_playlist(mut self) -> (Playlist, Vec<Song>) {
    self.playlist.songs = self.songs.iter().map(|song| song.id.clone()).collect();
    (self.playlist, self.songs)
  }
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct Status {
  state: Playback,
}

fn catalog_call<T>(
  socket: &Path,
  f: impl FnOnce(&mut dyn FnMut(Call, Vec<Song>) -> MediaResult<(Reply, Lists)>) -> MediaResult<T>,
) -> MediaResult<T> {
  let stream = UnixStream::connect(socket)?;
  stream.set_read_timeout(Some(Duration::from_secs(20)))?;
  stream.set_write_timeout(Some(Duration::from_secs(4)))?;
  let mut input = StreamDeserializer::new(&stream);
  let mut output = StreamSerializer::new_with_write(&stream);
  output.serialize(&greeting(3, false))?;
  peer(&mut input)?;
  let mut id = 0;
  f(&mut |call, songs| {
    id += 1;
    let part_size = songs.len().div_ceil(16).max(256);
    for part in songs.chunks(part_size) {
      output.serialize(&Outgoing {
        id,
        call: None,
        ahead: Some(SongPart {
          songs: part.to_vec(),
        }),
      })?;
    }
    output.serialize(&Outgoing {
      id,
      call: Some(call),
      ahead: None,
    })?;
    let mut parts = Lists::default();
    while let Some(frame) = input.deserialize::<Frame>()? {
      if frame.id != id {
        continue;
      }
      if let Some(mut ahead) = frame.ahead {
        parts.songs.append(&mut ahead.songs);
      }
      if let Some(mut reply) = frame.reply {
        if let Some(error) = reply.err.take() {
          return Err(
            match error.code {
              1 => "Playlist or editable track not found",
              2 => "Sign in to see saved playlists",
              3 => "YouTube Music sign-in expired",
              _ => "The player could not load this request",
            }
            .into(),
          );
        }
        return Ok((reply, parts));
      }
    }
    Err("player disconnected".into())
  })
}

fn library_request<T: serde::de::DeserializeOwned>(args: &[&str]) -> MediaResult<T> {
  let output = Command::new("lintel-ytmusic").args(args).output()?;
  if !output.status.success() {
    let details = String::from_utf8_lossy(&output.stderr);
    return Err(
      details
        .trim()
        .lines()
        .last()
        .filter(|line| !line.is_empty())
        .unwrap_or("YouTube Music request failed.")
        .to_owned()
        .into(),
    );
  }
  Ok(serde_json::from_slice(&output.stdout)?)
}

pub enum Library {
  Connected(Vec<Playlist>),
  Disconnected,
  Expired,
}

pub fn cached_library() -> Option<Vec<Playlist>> {
  let config = gio::glib::user_config_dir().join("lintel");
  if !config.join("youtube-browser.json").is_file()
    && !(config.join("youtube-client.json").is_file()
      && config.join("youtube-oauth.json").is_file())
  {
    return None;
  }
  let cache = gio::glib::user_cache_dir().join("lintel/library.json");
  serde_json::from_slice(&fs::read(cache).ok()?).ok()
}

pub fn playlists() -> MediaResult<Library> {
  let output = Command::new("lintel-ytmusic").arg("library").output()?;
  match output.status.code() {
    Some(0) => Ok(Library::Connected(serde_json::from_slice(&output.stdout)?)),
    Some(3) => Ok(Library::Disconnected),
    Some(4) => Ok(Library::Expired),
    _ => Err("Could not load your YouTube Music library".into()),
  }
}

pub fn session_command(command: &str) -> MediaResult<()> {
  let output = Command::new("lintel-ytmusic").arg(command).output()?;
  if !output.status.success() {
    return Err(
      String::from_utf8_lossy(&output.stderr)
        .trim()
        .to_owned()
        .into(),
    );
  }
  Ok(())
}

pub fn sign_in(window_closed: impl FnOnce()) -> MediaResult<Library> {
  let mut child = Command::new("lintel-ytmusic")
    .args(["login", "--library"])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()?;
  let mut first = [0];
  let read = child.stdout.as_mut().unwrap().read(&mut first);
  // An empty line identifies the point where the visible window closed.
  if matches!(read, Ok(1)) && first[0] == b'\n' {
    window_closed();
  }
  let mut output = child.wait_with_output()?;
  if !output.status.success() {
    return Err(
      String::from_utf8_lossy(&output.stderr)
        .trim()
        .to_owned()
        .into(),
    );
  }
  if read? == 1 && first[0] != b'\n' {
    output.stdout.insert(0, first[0]);
  }
  Ok(Library::Connected(serde_json::from_slice(&output.stdout)?))
}

fn load_playlist(
  id: &str,
  prefer_cache: bool,
  first_page: bool,
) -> MediaResult<(Playlist, Vec<Song>)> {
  let mut args = vec!["playlist", id];
  if prefer_cache {
    args.push("--prefer-cache");
  }
  if first_page {
    args.push("--first-page");
  }
  Ok(library_request::<PlaylistReply>(&args)?.into_playlist())
}

pub struct PreparedPlaylist {
  playlist: Playlist,
  songs: Vec<Song>,
  selection: QueueSelection,
}

impl PreparedPlaylist {
  pub fn first_song(&self) -> Option<(&Song, f64)> {
    let id = self.selection.ids.get(self.selection.start as usize)?;
    self
      .songs
      .iter()
      .find(|song| &song.id == id)
      .map(|song| (song, self.selection.position))
  }

  pub fn play(self, socket: &Path) -> MediaResult<Playlist> {
    if !socket.exists() && !run_control(&["daemon", "start"]) {
      return Err("Could not start the music player".into());
    }
    catalog_call(socket, |call| {
      if !self.selection.ordered {
        let (reply, _) = call(
          Call {
            method: STATUS,
            ..Call::default()
          },
          vec![],
        )?;
        if !reply
          .status
          .ok_or("player did not return its playback state")?
          .state
          .shuffled
        {
          call(
            Call {
              method: TOGGLE_SHUFFLE,
              ..Call::default()
            },
            vec![],
          )?;
        }
      }
      call(
        Call {
          method: PLAY_ALL,
          ids: Some(self.selection),
          ..Call::default()
        },
        self.songs,
      )?;
      Ok(self.playlist)
    })
  }
}

pub fn prepare_playlist(
  playlist: Playlist,
  bookmark: Option<Bookmark>,
) -> MediaResult<PreparedPlaylist> {
  let (playlist, songs) = load_playlist(&playlist.id, true, bookmark.is_none())?;
  let mut ids = playlist.songs.clone();
  if ids.is_empty() {
    return Err("This playlist has no playable tracks".into());
  }
  let mut position = 0.0;
  let start = if let Some(bookmark) = &bookmark {
    let mut remaining: HashSet<_> = ids.iter().collect();
    let mut restored: Vec<_> = bookmark
      .order
      .iter()
      .filter(|id| remaining.remove(id))
      .cloned()
      .collect();
    let exact = restored.iter().position(|id| *id == bookmark.id);
    let index = exact.or_else(|| {
      let previous = bookmark.order.iter().position(|id| *id == bookmark.id)?;
      bookmark
        .order
        .iter()
        .skip(previous + 1)
        .find_map(|next| restored.iter().position(|id| id == next))
    });
    if let Some(index) = index {
      if exact.is_some() {
        position = bookmark.position;
      }
      let mut added: Vec<_> = ids
        .iter()
        .filter(|id| remaining.remove(id))
        .cloned()
        .collect();
      for i in (1..added.len()).rev() {
        added.swap(
          i,
          gio::glib::random_int_range(0, i32::try_from(i + 1)?) as usize,
        );
      }
      restored.extend(added);
      ids = restored;
      i32::try_from(index)?
    } else {
      for i in (1..ids.len()).rev() {
        ids.swap(
          i,
          gio::glib::random_int_range(0, i32::try_from(i + 1)?) as usize,
        );
      }
      0
    }
  } else {
    gio::glib::random_int_range(0, i32::try_from(ids.len())?)
  };
  Ok(PreparedPlaylist {
    playlist,
    songs,
    selection: QueueSelection {
      ids,
      start,
      expected: Vec::new(),
      ordered: bookmark.is_some(),
      position,
    },
  })
}

pub fn refresh_playlist(
  socket: &Path,
  previous: Playlist,
  queue: Arc<Vec<String>>,
) -> MediaResult<Playlist> {
  let (playlist, songs) = load_playlist(&previous.id, false, false)?;
  sync_playlist_queue(socket, &previous, &playlist, &queue, songs)?;
  Ok(playlist)
}

fn sync_playlist_queue(
  socket: &Path,
  previous: &Playlist,
  playlist: &Playlist,
  queue: &[String],
  songs: Vec<Song>,
) -> MediaResult<()> {
  let old_ids: HashSet<_> = previous.songs.iter().collect();
  let new_ids: HashSet<_> = playlist.songs.iter().collect();
  // Adapt the playing queue only if it still represents this playlist.
  if old_ids == new_ids
    || old_ids.len() != queue.len()
    || !queue.iter().all(|id| old_ids.contains(id))
  {
    return Ok(());
  }
  catalog_call(socket, |call| {
    call(
      Call {
        method: SYNC_PLAYLIST_QUEUE,
        ids: Some(QueueSelection {
          ids: playlist.songs.clone(),
          expected: queue.to_vec(),
          start: 0,
          ordered: false,
          position: 0.0,
        }),
        ..Call::default()
      },
      songs,
    )?;
    Ok(())
  })
}

pub fn remove_song(
  socket: &Path,
  previous: Playlist,
  queue: Arc<Vec<String>>,
  song: &str,
) -> MediaResult<Playlist> {
  let output = Command::new("lintel-ytmusic")
    .args(["remove", previous.id.as_str(), song])
    .output()?;
  if !output.status.success() {
    return Err(
      String::from_utf8_lossy(&output.stderr)
        .trim()
        .to_owned()
        .into(),
    );
  }
  let (playlist, songs) = serde_json::from_slice::<PlaylistReply>(&output.stdout)?.into_playlist();
  // The remote edit has already succeeded even if the local queue changed.
  if let Err(error) = sync_playlist_queue(socket, &previous, &playlist, &queue, songs) {
    eprintln!("Playlist queue: {error}");
  }
  Ok(playlist)
}

pub fn add_song(playlist: &str, song: &str) -> MediaResult<()> {
  library_request(&["add", playlist, song])
}

pub fn search(query: &str) -> MediaResult<Vec<Song>> {
  library_request(&["search", query])
}

#[derive(Default, Deserialize)]
pub struct Lyrics {
  #[serde(default)]
  pub instrumental: bool,
  pub lines: Vec<LyricLine>,
}

#[derive(Deserialize)]
pub struct LyricLine {
  pub text: String,
  pub start: Option<f64>,
  pub end: Option<f64>,
  #[serde(default)]
  pub words: Vec<LyricWord>,
}

#[derive(Deserialize)]
pub struct LyricWord {
  pub start: f64,
  #[serde(default)]
  pub end: Option<f64>,
  pub end_byte: u32,
}

pub fn lyrics(id: &str, title: &str, artist: &str, duration: f64) -> MediaResult<Option<Lyrics>> {
  let mut result: Option<Lyrics> = library_request(&[
    "lyrics",
    id,
    &format!("--title={title}"),
    &format!("--artist={artist}"),
    &format!("--duration={duration}"),
  ])?;
  if let Some(lyrics) = &mut result
    && lyrics.lines.iter().all(|line| line.start.is_some())
  {
    lyrics
      .lines
      .sort_by(|a, b| a.start.unwrap().total_cmp(&b.start.unwrap()));
  }
  Ok(result)
}

pub fn play_result(socket: &Path, song: Song) -> MediaResult<()> {
  if !socket.exists() && !run_control(&["daemon", "start"]) {
    return Err("Could not start the music player".into());
  }
  catalog_call(socket, |call| {
    call(
      Call {
        method: PLAY_ALL,
        ids: Some(QueueSelection {
          ids: vec![song.id.clone()],
          expected: Vec::new(),
          start: 0,
          ordered: false,
          position: 0.0,
        }),
        ..Call::default()
      },
      vec![song],
    )?;
    Ok(())
  })
}

pub fn move_queue(socket: &Path, expected: Vec<String>, from: usize, to: usize) -> MediaResult<()> {
  catalog_call(socket, |call| {
    call(
      Call {
        method: MOVE,
        move_args: Some(MoveArgs {
          from: i32::try_from(from)?,
          to: i32::try_from(to)?,
          expected,
        }),
        ..Call::default()
      },
      Vec::new(),
    )?;
    Ok(())
  })
}

pub fn artwork_accent(art: &Art) -> Option<u32> {
  let mut sums = [0_u64; 3];
  let mut weight_sum = 0_u64;
  for px in art.data.chunks_exact(4).step_by(11) {
    let [blue, green, red, alpha] = [px[0], px[1], px[2], px[3]];
    let peak = red.max(green).max(blue);
    let chroma = peak - red.min(green).min(blue);
    if alpha < 220 || chroma < 24 || peak < 64 {
      continue;
    }
    let weight = u64::from(chroma).pow(2);
    for (sum, channel) in sums.iter_mut().zip([red, green, blue]) {
      *sum += u64::from(channel) * weight;
    }
    weight_sum += weight;
  }
  if weight_sum == 0 {
    return None;
  }
  let channels = sums.map(|sum| sum / weight_sum);
  let peak = channels[0].max(channels[1]).max(channels[2]);
  Some(channels.into_iter().fold(0_u32, |rgb, channel| {
    (rgb << 8) | (144 + channel * 101 / peak) as u32
  }))
}

pub fn artwork(url: &str, size: u32) -> Option<Art> {
  let mut url = url.to_owned();
  let host = url.strip_prefix("https://")?.split('/').next()?;
  if matches!(
    host,
    "lh3.googleusercontent.com" | "yt3.googleusercontent.com"
  ) {
    if let Some(suffix) = url.rfind('=') {
      url.truncate(suffix);
    }
    url.push_str(&format!("=w{size}-h{size}-rj"));
  }
  let response = Command::new("curl")
    .args([
      "--fail",
      "--silent",
      "--max-time",
      "6",
      "--max-filesize",
      "2097152",
      "--",
      &url,
    ])
    .output()
    .ok()?;
  if !response.status.success() {
    return None;
  }
  decode_art(response.stdout, size)
}

pub fn decode_art(bytes: Vec<u8>, size: u32) -> Option<Art> {
  let mut reader = ImageReader::new(Cursor::new(bytes))
    .with_guessed_format()
    .ok()?;
  let mut limits = Limits::default();
  limits.max_image_width = Some(2048);
  limits.max_image_height = Some(2048);
  limits.max_alloc = Some(20 << 20);
  reader.limits(limits);
  Some(thumbnail_art(reader.decode().ok()?, size))
}

pub fn thumbnail_art(image: image::DynamicImage, size: u32) -> Art {
  let pixels = image.thumbnail(size, size).into_rgba8();
  let (width, height) = (pixels.width() as i32, pixels.height() as i32);
  let mut data = pixels.into_raw();
  for px in data.chunks_exact_mut(4) {
    let alpha = u16::from(px[3]);
    px.swap(0, 2);
    for channel in &mut px[..3] {
      *channel = (u16::from(*channel) * alpha / 255) as u8;
    }
  }
  Art {
    data,
    width,
    height,
  }
}
