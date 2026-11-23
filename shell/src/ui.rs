use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cairo::{Context, Format, ImageSurface, Operator};
use gio::glib::DateTime;
use pango::{Alignment, EllipsizeMode, FontDescription, SCALE, Weight};

use crate::audio::{Device, MusicChannel, Stream, StreamId, Volume};
use crate::media::{Art, Bookmark, Lyrics, Playlist, QueueSong, Song, Track};

const HEADER_WIDTH: i32 = 408;
pub const WIDTH: i32 = 644;
pub const HEADER_X: i32 = (WIDTH - HEADER_WIDTH) / 2;
const MUSIC_PANEL_X: i32 = 18;
const MUSIC_PANEL_WIDTH: i32 = WIDTH - 2 * MUSIC_PANEL_X;
const MUSIC_LEFT: i32 = MUSIC_PANEL_X + 18;
pub const HEIGHT: i32 = 40;
pub const WING: i32 = 18;
pub const HISTORY_X: i32 = HEADER_X + WING + 8;
pub const UTILITY_WIDTH: i32 = 40;
pub const TRAY_X: i32 = HISTORY_X + UTILITY_WIDTH;
pub const MUSIC_X: i32 = WIDTH - HEADER_X - WING - UTILITY_WIDTH - 8;
pub const AUDIO_X: i32 = MUSIC_X - UTILITY_WIDTH;
const BUTTON_WIDTH: i32 = 40;
const PLAYER_X: i32 = MUSIC_PANEL_X + 340;
const PLAYER_WIDTH: i32 = MUSIC_PANEL_WIDTH - 340;
const TRANSPORT_X: i32 = PLAYER_X + (PLAYER_WIDTH - 3 * BUTTON_WIDTH) / 2;
const SEEK_Y: i32 = 294;
const TRANSPORT_Y: i32 = 313;
const TAB_Y: i32 = PANEL_Y + 17;
const TAB_START: i32 = MUSIC_LEFT - 4;
const TAB_WIDTH: i32 = 28;
const MUSIC_TAB_WIDTH: i32 = 81;
const MUSIC_TAB_HEIGHT: i32 = 32;
const SAVE_WIDTH: i32 = 55;
const SAVE_X: i32 = PLAYER_X - SAVE_WIDTH - 12;
const LIST_Y: i32 = PANEL_Y + 60;
const MUSIC_BOTTOM: i32 = TRANSPORT_END + 24;
const TAB_END: i32 = MUSIC_BOTTOM - 3;
const SEARCH_END: i32 = TAB_END - 22;
const MUSIC_GAIN_TOP: i32 = PANEL_Y + 92;
const MUSIC_GAIN_BOTTOM: i32 = PANEL_Y + 197;
const LYRICS_SHIFT: i32 = 174;
const LYRICS_LEFT: i32 = MUSIC_LEFT + 12;
const LYRICS_WIDTH: i32 = WIDTH - 2 * LYRICS_LEFT;
const LYRICS_TOP: i32 = LIST_Y + 20;
const LYRICS_BOTTOM: i32 = 473;
const LYRICS_FOOTER_Y: i32 = 480;
const RESULTS_Y: i32 = LIST_Y + 41;
const RESULTS_ROW: i32 = 46;
const SEARCH_ROWS: usize = ((SEARCH_END - RESULTS_Y) / RESULTS_ROW) as usize;
const QUEUE_ROW: i32 = 42;
const PLAYLIST_ROW: i32 = 42;
const PLAYLIST_ROWS: usize = ((TAB_END - LIST_Y) / PLAYLIST_ROW) as usize;
const QUEUE_ROWS: usize = ((TAB_END - LIST_Y) / QUEUE_ROW) as usize;
const TRANSPORT_END: i32 = PANEL_Y + TRANSPORT_Y + 30;
pub const CORNER_SIZE: i32 = 20;
pub const PANEL_WIDTH: i32 = HEADER_WIDTH - 2 * WING;
const PANEL_X: i32 = (WIDTH - PANEL_WIDTH) / 2;
const PANEL_Y: i32 = HEIGHT;
const COVER: i32 = 160;

const ROW_HEIGHT: i32 = 40;
const VISIBLE_ROWS: usize = 6;
pub const POPUP_WIDTH: i32 = 340;
pub const POPUP_HEIGHT: i32 = 112;
const POPUP_GAP: i32 = 8;
const POPUP_LIMIT: usize = 3;
pub const POPUP_RADIUS: f64 = 14.0;
pub const REVEAL_HEIGHT: i32 = 2;
pub const SHADOW_DEPTH: i32 = 8;
const SIGN_IN_X: i32 = PANEL_X + 16;
const SIGN_IN_Y: i32 = PANEL_Y + 12;
const SIGN_IN_WIDTH: i32 = PANEL_WIDTH - 32;
const SIGN_IN_BUTTON_HEIGHT: i32 = 38;
const SIGN_OUT_Y: i32 = TAB_Y;
const PLAYER_SIGN_OUT_X: i32 = PLAYER_X + PLAYER_WIDTH - TAB_WIDTH - 12;
const PLAYER_REMOVE_X: i32 = PLAYER_X + PLAYER_WIDTH - TAB_WIDTH - 15;
const SIGN_OUT_WIDTH: i32 = 38;
const RECONNECT_WIDTH: i32 = SIGN_IN_WIDTH - SIGN_OUT_WIDTH - 8;
const VOLUME_FEEDBACK_HEIGHT: i32 = 32;
const POPUP_CLOSE_X: i32 = POPUP_WIDTH - 34;
const POPUP_CLOSE_Y: i32 = 10;
const POPUP_CLOSE_SIZE: i32 = 24;
const AUDIO_NAME_X: i32 = PANEL_X + 43;
const AUDIO_BUTTON_SIZE: i32 = 28;
const AUDIO_BUTTON_X: i32 = AUDIO_NAME_X - 33;
const AUDIO_DEVICES: [Device; 2] = [Device::Output, Device::Input];
const APP_AUDIO_Y: i32 = 135;

#[derive(Clone)]
pub struct Notice {
  pub id: u32,
  pub summary: String,
  pub body: String,
  pub source: Option<String>,
  pub actions: Vec<(String, String)>,
  pub live: bool,
  pub popup: bool,
  pub seen: bool,
  pub image: Option<ImageSurface>,
  pub transient: bool,
  pub resident: bool,
  pub countdown: Option<Countdown>,
}

#[derive(Clone)]
pub struct Countdown {
  duration: Duration,
  remaining: Duration,
  started: Option<Instant>,
}

impl Countdown {
  pub fn new(duration: Duration) -> Self {
    Self {
      duration,
      remaining: duration,
      started: Some(Instant::now()),
    }
  }

  fn remaining(&self) -> Duration {
    self
      .remaining
      .saturating_sub(self.started.map_or(Duration::ZERO, |start| start.elapsed()))
  }

  pub fn pause(&mut self) {
    self.remaining = self.remaining();
    self.started = None;
  }

  pub fn resume(&mut self) {
    if self.started.is_none() {
      self.started = Some(Instant::now());
    }
  }

  pub fn expiry_delay(&self) -> Option<Duration> {
    self.started.map(|_| self.remaining())
  }
}

impl Notice {
  fn activate(&self) -> Option<Action> {
    (self.live && self.actions.iter().any(|(key, _)| key == "default"))
      .then(|| Action::Invoke(self.id, "default".into()))
  }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Address {
  pub name: String,
  pub path: String,
}

pub struct TrayItem {
  pub address: Address,
  pub title: String,
  pub image: Option<ImageSurface>,
  pub menu_only: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Panel {
  #[default]
  None,
  Music,
  History,
  Tray,
  Audio,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Layer {
  Bar,
  Popup,
  Corner(u32, u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MusicTab {
  #[default]
  Playlists,
  Queue,
  Lyrics,
  Search,
}

const MUSIC_TABS: [MusicTab; 4] = [
  MusicTab::Playlists,
  MusicTab::Queue,
  MusicTab::Lyrics,
  MusicTab::Search,
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct LyricMarker {
  index: usize,
  word_end: u32,
  word_progress: u8,
  scroll_progress: u16,
  singing: bool,
}

struct PlaylistTarget {
  song: Song,
  results_offset: usize,
  search_row: Option<usize>,
}

pub struct Removal {
  pub playlist: String,
  pub song: String,
  title: String,
}

pub enum SignInStage {
  Browser,
  Library,
}

enum MusicConfirmation {
  Remove(Removal),
  SignOut,
}

struct QueueDrag {
  expected: Arc<Vec<String>>,
  playing: usize,
  from: usize,
  to: usize,
  submitted: bool,
}

impl QueueDrag {
  fn source_index(&self, row: usize) -> usize {
    if row == self.to {
      self.from
    } else if (self.from..self.to).contains(&row) {
      row + 1
    } else if (self.to + 1..=self.from).contains(&row) {
      row - 1
    } else {
      row
    }
  }
}

pub enum Action {
  Media {
    command: &'static str,
    rewind: bool,
  },
  OpenMusic,
  SmallCovers,
  OpenAudio,
  Seek(f64),
  Play(Playlist),
  RemoveSong(Removal),
  Reorder {
    expected: Arc<Vec<String>>,
    from: usize,
    to: usize,
  },
  MusicTab(MusicTab),
  Search(String),
  PlayResult(Song),
  AddSong(Playlist, Song),
  DismissBar,
  BarFocusLost,
  SignIn,
  SignOut,
  HideBar(u64),
  Volume(Slider, Option<u8>),
  ToggleMic,
  ToggleMonitor,
  ToggleDoNotDisturb,
  ClearNotifications,
  Close(u32),
  PopupHover,
  Invoke(u32, String),
  Tray(Address, &'static str),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Slider {
  Device(Device),
  Stream(StreamId),
  Music(StreamId),
  MusicLevel(MusicChannel, u64),
}

impl Slider {
  fn start(self) -> i32 {
    match self {
      Self::Device(_) => AUDIO_NAME_X,
      Self::Stream(_) => SLIDER_X,
      Self::Music(_) | Self::MusicLevel(..) => unreachable!("vertical music slider"),
    }
  }

  pub fn position(self, x: i32, y: i32) -> u8 {
    if matches!(self, Self::Music(_) | Self::MusicLevel(..)) {
      return ((MUSIC_GAIN_BOTTOM - y) * 100 / (MUSIC_GAIN_BOTTOM - MUSIC_GAIN_TOP)).clamp(0, 100)
        as u8;
    }
    ((x - self.start()) * 100 / (SLIDER_END - self.start())).clamp(0, 100) as u8
  }

  fn pick(self, x: i32, mute: bool) -> Option<Action> {
    if mute {
      Some(Action::Volume(self, None))
    } else {
      (x >= self.start()).then(|| Action::Volume(self, Some(self.position(x, 0))))
    }
  }
}

#[derive(Clone, Copy)]
pub enum AudioChange {
  Slider(Slider, Option<u8>),
  OutputStep(i32),
}

impl AudioChange {
  pub fn target(self) -> Slider {
    match self {
      Self::Slider(target, _) => target,
      Self::OutputStep(_) => Slider::Device(Device::Output),
    }
  }
}

#[derive(Default)]
struct Hover {
  from: f64,
  active: bool,
  started: Option<Instant>,
}

const HOVER_DURATION: Duration = Duration::from_millis(100);
const BUTTONS: [i32; 4] = [HISTORY_X, TRAY_X, AUDIO_X, MUSIC_X];

impl Hover {
  fn update(&mut self, active: bool) -> bool {
    if active == self.active {
      return false;
    }
    self.from = self.opacity();
    self.active = active;
    self.started = Some(Instant::now());
    true
  }

  fn animating(&self) -> bool {
    self
      .started
      .is_some_and(|start| start.elapsed() < HOVER_DURATION)
  }

  fn opacity(&self) -> f64 {
    let target = f64::from(u8::from(self.active));
    let progress = self.started.map_or(1.0, |started| {
      (started.elapsed().as_secs_f64() / HOVER_DURATION.as_secs_f64()).min(1.0)
    });
    self.from + (target - self.from) * progress
  }
}

#[derive(Default)]
pub struct Model {
  pub track: Track,
  pub notices: VecDeque<Notice>,
  pub do_not_disturb: bool,
  pub trays: Vec<TrayItem>,
  pub panel: Panel,
  pub playlists: Vec<Playlist>,
  pub playlist_busy: bool,
  pub library_refreshing: bool,
  pub library_revision: u64,
  pub library_loaded: Option<Instant>,
  pub small_covers: HashMap<String, Option<ImageSurface>>,
  pub selected_playlist: Option<Playlist>,
  pub loading_playlist: Option<String>,
  pub starting_song: Option<(Song, f64)>,
  pub playlist_bookmarks: HashMap<String, Bookmark>,
  pub music_clock: bool,
  pub music_tab: MusicTab,
  pub search_query: String,
  pub search_cursor: usize,
  pub search_cursor_visible: bool,
  pub search_results: Vec<Song>,
  pub search_status: Option<String>,
  pub search_pending: Option<Instant>,
  pub search_revision: u64,
  search_row: Option<usize>,
  playlist_target: Option<PlaylistTarget>,
  pub adding_song: bool,
  pub add_feedback: Option<String>,
  pub lyrics_id: String,
  pub lyrics_metadata: bool,
  pub lyrics: Lyrics,
  lyrics_scrolled: bool,
  lyric_hits: RefCell<Vec<(i32, i32, f64)>>,
  pub lyrics_status: Option<String>,
  seek_preview: Option<(String, f64)>,
  pub pending_seek: Option<(String, f64)>,
  queue_drag: Option<QueueDrag>,
  confirmation: Option<MusicConfirmation>,
  pub signed_in: bool,
  pub library_checked: bool,
  pub reconnect: bool,
  pub signing_in: Option<SignInStage>,
  pub(crate) bar_visible: bool,
  pub(crate) pointer_generation: u64,
  pub opacity: f64,
  hover: [Hover; 4],
  sign_in_hover: Hover,
  audio_hover: [Hover; 2],
  pub playlist_epoch: u64,
  pub playlist_syncing: bool,
  pub playlist_error: Option<String>,
  pub output: Option<Volume>,
  pub input: Option<Volume>,
  pub streams: Vec<Stream>,
  icons: HashMap<String, Option<ImageSurface>>,
  pub slider_preview: Option<(Slider, u8)>,
  pub audio_watching: bool,
  pub music_mix: bool,
  pub music_levels: [Option<(u64, Volume)>; 2],
  pub audio_requests: VecDeque<AudioChange>,
  pub volume_feedback: Option<(Instant, Volume)>,
  popup_pointer: Option<(f64, f64)>,
  popup_close_hover: Option<u32>,
  bar_pointer: Option<(f64, f64)>,
  title_scroll: Cell<Option<Instant>>,
  row_scroll: Cell<Option<(usize, Instant)>>,
  title_animating: Cell<bool>,
  row_animating: Cell<bool>,
  pub audio_setting: bool,
  pub audio_revision: u64,
  pub mic_route: bool,
  pub mic_pending: bool,
  pub monitoring: bool,
  pub mic_error: Option<String>,
  pub mic_revision: u64,
  pub audio_error: Option<String>,
  pub large_cover: Option<(String, Option<ImageSurface>)>,
  offset: usize,
}

impl Model {
  pub fn set_library(&mut self, pinned: &[Playlist], playlists: Vec<Playlist>) {
    self.signed_in = true;
    self.library_checked = true;
    self.reconnect = false;
    let visible = |list: &Playlist| list.id.trim_start_matches("VL") != "SE";
    self.playlists = pinned
      .iter()
      .filter(|list| visible(list))
      .cloned()
      .collect();
    for playlist in playlists.into_iter().filter(visible) {
      if let Some(pin) = self.playlists.iter_mut().find(|pin| pin.id == playlist.id) {
        pin.art = playlist.art;
      } else {
        self.playlists.push(playlist);
      }
    }
  }

  pub fn hover_bar(&mut self, pointer: Option<(f64, f64)>) -> bool {
    let mut changed = matches!(self.panel, Panel::Music | Panel::History | Panel::Tray)
      && self.bar_pointer != pointer;
    changed |= self.panel == Panel::Audio && monitor_hit(self.bar_pointer) != monitor_hit(pointer);
    self.bar_pointer = pointer;
    if pointer.is_none() {
      self.row_scroll.set(None);
    }
    for (left, hover) in BUTTONS.into_iter().zip(&mut self.hover) {
      let active = pointer.is_some_and(|(x, y)| {
        y >= 0.0
          && y < f64::from(HEIGHT)
          && (f64::from(left)..f64::from(left + UTILITY_WIDTH)).contains(&x)
      });
      changed |= hover.update(active);
    }
    let sign_in = self.panel == Panel::Music
      && !self.signed_in
      && !self.library_refreshing
      && !self.playlist_busy
      && pointer.is_some_and(|(x, y)| self.sign_in_hit(x as i32, y as i32));
    changed |= self.sign_in_hover.update(sign_in);
    for (index, hover) in self.audio_hover.iter_mut().enumerate() {
      let active = match self.panel {
        Panel::Audio => audio_button_hit(pointer, index),
        Panel::Music
          if self.signed_in
            && self.music_tab != MusicTab::Lyrics
            && self.confirmation.is_none() =>
        {
          if index == MusicChannel::Listen as usize {
            music_gain_icon_hit(pointer, MusicChannel::Listen)
          } else {
            self.music_mix && music_gain_icon_hit(pointer, MusicChannel::Send)
          }
        }
        _ => false,
      };
      changed |= hover.update(active);
    }
    changed
  }

  pub fn header_hover_animating(&self) -> bool {
    self.hover.iter().any(Hover::animating)
  }

  pub fn panel_hover_animating(&self) -> bool {
    self.sign_in_hover.animating() || self.audio_hover.iter().any(Hover::animating)
  }

  pub fn timeline_animating(&self) -> bool {
    self.signed_in && self.confirmation.is_none() && self.track.progressing_since.is_some()
  }

  pub fn set_streams(&mut self, streams: Vec<Stream>, music_only: bool) -> bool {
    if music_only {
      if self.music_stream() == streams.first() {
        return false;
      }
      self.streams.retain(|stream| !stream.music);
      self.streams.extend(streams);
      return true;
    }
    if self.streams == streams {
      return false;
    }
    self.icons.retain(|name, _| {
      streams
        .iter()
        .any(|stream| !stream.music && stream.icon == *name)
    });
    for stream in streams.iter().filter(|stream| !stream.music) {
      self
        .icons
        .entry(stream.icon.clone())
        .or_insert_with(|| app_icon(&stream.icon));
    }
    self.streams = streams;
    true
  }

  fn playback_position(&self) -> f64 {
    self
      .seek_preview
      .as_ref()
      .or(self.pending_seek.as_ref())
      .filter(|(id, _)| *id == self.track.id)
      .map_or_else(|| self.track.elapsed(), |(_, pos)| *pos)
  }

  pub fn lyrics_view(&self) -> bool {
    self.wide_music() && self.music_tab == MusicTab::Lyrics
  }

  pub fn music_marquee(&self, region: BarRegion) -> bool {
    self.wide_music()
      && match region {
        BarRegion::MusicTitle => self.title_animating.get(),
        BarRegion::MusicList => !self.lyrics_view() && self.row_animating.get(),
        _ => false,
      }
  }

  fn player_shift(&self) -> i32 {
    if self.lyrics_view() { LYRICS_SHIFT } else { 0 }
  }

  pub fn lyric_marker(&self) -> Option<LyricMarker> {
    if !self.lyrics_view()
      || self.lyrics_id != self.track.id
      || self.lyrics.lines.first()?.start.is_none()
    {
      return None;
    }
    let elapsed = self.playback_position();
    let lines = &self.lyrics.lines;
    let started = lines.partition_point(|line| line.start.is_some_and(|start| start <= elapsed));
    let index = started.saturating_sub(1);
    let line = &lines[index];
    let word = line
      .words
      .iter()
      .take_while(|word| word.start <= elapsed)
      .last();
    let progress = word.map_or(0, |word| {
      let span = word.end.unwrap_or(word.start) - word.start;
      if span <= 0.0 {
        16
      } else {
        ((elapsed - word.start) / span * 16.0).clamp(0.0, 16.0) as u8
      }
    });
    let scroll_progress = if !self.lyrics_scrolled && self.track.progressing_since.is_some() {
      line.start.map_or(1024, |start| {
        (((elapsed - start) / 0.28) * 1024.0).clamp(0.0, 1024.0) as u16
      })
    } else {
      1024
    };
    Some(LyricMarker {
      index,
      word_end: word.map_or(0, |word| word.end_byte),
      word_progress: progress,
      scroll_progress,
      singing: started > 0 && line.end.is_none_or(|end| elapsed <= end),
    })
  }

  pub fn animated_lyric_marker(&self) -> Option<LyricMarker> {
    let marker = self.lyric_marker()?;
    let start = self.lyrics.lines[marker.index].start?;
    (!self.lyrics_scrolled
      || self
        .lyric_hits
        .borrow()
        .iter()
        .any(|&(_, _, time)| time == start))
    .then_some(marker)
  }

  pub fn lyric_scroll_animating(&self) -> bool {
    self.lyric_marker().is_some_and(|marker| {
      marker.index > 0 && marker.scroll_progress < 1024 && !self.lyrics_scrolled
    })
  }

  fn music_stream(&self) -> Option<&Stream> {
    self.streams.iter().find(|stream| stream.music)
  }

  fn music_slider(&self, channel: MusicChannel) -> Option<(Slider, Volume)> {
    if self.music_mix {
      let (id, volume) = self.music_levels[channel as usize]?;
      Some((Slider::MusicLevel(channel, id), volume))
    } else if channel == MusicChannel::Listen {
      let stream = self.music_stream()?;
      Some((Slider::Music(stream.id), stream.volume))
    } else {
      None
    }
  }

  fn app_streams(&self) -> impl Iterator<Item = &Stream> {
    self.streams.iter().filter(|stream| !stream.music)
  }

  fn volume(&self, target: Slider, volume: Option<Volume>) -> Option<Volume> {
    if let Some((slider, percent)) = self.slider_preview
      && slider == target
    {
      return Some(Volume {
        percent,
        muted: volume.is_some_and(|volume| volume.muted),
      });
    }
    volume
  }

  pub fn select(&mut self, panel: Panel) {
    if self.panel == Panel::History && panel != Panel::History {
      self.mark_visible_notices_seen();
    }
    self.panel = panel;
    self.title_scroll.set(None);
    self.row_scroll.set(None);
    self.playlist_target = None;
    self.search_row = None;
    self.lyrics_scrolled = false;
    self.audio_hover = Default::default();
    self.offset = 0;
    self.confirmation = None;
    self.seek_preview = None;
    self.queue_drag = None;
    self.slider_preview = None;
  }

  pub fn dismiss_confirmation(&mut self) -> bool {
    if self.confirmation.take().is_some() {
      return true;
    }
    self.close_playlist_picker().is_some()
  }

  fn close_playlist_picker(&mut self) -> Option<Song> {
    let target = self.playlist_target.take()?;
    self.offset = target.results_offset;
    self.search_row = target.search_row;
    Some(target.song)
  }

  pub fn bar_hovered(&self) -> bool {
    self.bar_pointer.is_some()
  }

  fn count(&self) -> usize {
    match self.panel {
      Panel::History => self.history().count(),
      Panel::Tray => self.trays.len(),
      Panel::Music => match self.music_tab {
        MusicTab::Playlists => self.playlists.len(),
        MusicTab::Queue => self
          .track
          .queue
          .len()
          .saturating_sub(self.track.queue_index + 1),
        MusicTab::Lyrics => self.lyrics.lines.len(),
        MusicTab::Search if self.playlist_target.is_some() => self.playlist_targets().count(),
        MusicTab::Search => self.search_results.len(),
      },
      Panel::Audio => self.app_streams().count(),
      _ => 0,
    }
  }

  pub fn scroll(&mut self, amount: f64, x: i32, y: i32) {
    if self.lyrics_view() && !(LYRICS_TOP..LYRICS_BOTTOM).contains(&y) {
      return;
    }
    if self.panel == Panel::Music
      && !(MUSIC_LEFT - 6..if self.lyrics_view() {
        WIDTH - MUSIC_LEFT
      } else {
        PLAYER_X
      })
        .contains(&x)
    {
      return;
    }
    if matches!(
      self.panel,
      Panel::History | Panel::Tray | Panel::Music | Panel::Audio
    ) {
      if self.wide_music()
        && self.music_tab == MusicTab::Lyrics
        && !self.lyrics_scrolled
        && let Some(marker) = self.lyric_marker()
      {
        let anchor = marker.index.saturating_sub(1);
        if (amount < 0.0 && marker.index == 0)
          || (amount > 0.0 && anchor + 1 >= self.lyrics.lines.len())
        {
          return;
        }
        self.offset = anchor;
        self.lyrics_scrolled = true;
      }
      let offset = if amount > 0.0 {
        (self.offset + 1).min(self.count().saturating_sub(
          if self.panel == Panel::Music && self.music_tab == MusicTab::Lyrics {
            1
          } else {
            self.visible_list_rows()
          },
        ))
      } else {
        self.offset.saturating_sub(1)
      };
      if self.panel == Panel::History && offset != self.offset {
        self.mark_visible_notices_seen();
      }
      self.offset = offset;
      self.search_row = self
        .search_row
        .filter(|row| (offset..offset + SEARCH_ROWS).contains(row));
    }
  }

  fn popups(&self) -> impl Iterator<Item = &Notice> {
    self
      .notices
      .iter()
      .filter(|notice| notice.popup)
      .take(POPUP_LIMIT)
  }

  fn history(&self) -> impl Iterator<Item = &Notice> {
    self.notices.iter().filter(|notice| !notice.transient)
  }

  fn mark_visible_notices_seen(&mut self) {
    for notice in self
      .notices
      .iter_mut()
      .filter(|notice| !notice.transient)
      .skip(self.offset)
      .take(VISIBLE_ROWS)
    {
      notice.seen = true;
    }
  }

  pub fn hover_popup(&mut self, pointer: Option<(f64, f64)>) -> bool {
    let before = self.popup_at_pointer();
    self.popup_pointer = pointer;
    let after = self.popup_at_pointer();
    let close = after
      .filter(|(_, close)| *close)
      .and_then(|(row, _)| self.popups().nth(row))
      .map(|notice| notice.id);
    let changed = close != self.popup_close_hover;
    self.popup_close_hover = close;
    changed || after != before
  }

  fn popup_at_pointer(&self) -> Option<(usize, bool)> {
    let (x, y) = self.popup_pointer?;
    let stride = POPUP_HEIGHT + POPUP_GAP;
    let y = y.floor() as i32;
    let row = usize::try_from(y.div_euclid(stride)).ok()?;
    let y = y.rem_euclid(stride);
    (row < POPUP_LIMIT && y < POPUP_HEIGHT).then(|| (row, popup_close_hit(x.floor() as i32, y)))
  }

  pub fn hovered_notice(&self) -> Option<u32> {
    let (row, _) = self.popup_at_pointer()?;
    Some(self.popups().nth(row)?.id)
  }

  pub fn timed_popups(&self) -> bool {
    self.popups().any(|notice| {
      notice
        .countdown
        .as_ref()
        .is_some_and(|countdown| countdown.started.is_some())
    })
  }

  pub fn popup_rows(&self) -> impl Iterator<Item = i32> {
    self
      .popups()
      .enumerate()
      .map(|(row, _)| row as i32 * (POPUP_HEIGHT + POPUP_GAP))
  }

  pub fn apply_volume(&mut self, target: Slider, percent: Option<u8>, confirmed: Option<Volume>) {
    let volume = match target {
      Slider::Device(Device::Output) => self.output.as_mut(),
      Slider::Device(Device::Input) => self.input.as_mut(),
      Slider::MusicLevel(channel, id) => self.music_levels[channel as usize]
        .as_mut()
        .filter(|(node, _)| *node == id)
        .map(|(_, volume)| volume),
      Slider::Stream(id) | Slider::Music(id) => self
        .streams
        .iter_mut()
        .find(|stream| stream.id == id)
        .map(|stream| &mut stream.volume),
    };
    if let Some(volume) = volume {
      if let Some(confirmed) = confirmed {
        *volume = confirmed;
      } else if let Some(percent) = percent {
        volume.percent = percent;
      } else {
        volume.muted = !volume.muted;
      }
    }
  }

  pub fn wide_music(&self) -> bool {
    self.panel == Panel::Music && self.signed_in
  }

  pub fn bar_height(&self) -> i32 {
    if !self.bar_visible && self.volume_feedback.is_none() {
      return REVEAL_HEIGHT;
    }
    self.panel_bottom()
      + if self.volume_feedback.is_some() {
        VOLUME_FEEDBACK_HEIGHT
      } else {
        0
      }
  }

  fn panel_bottom(&self) -> i32 {
    match self.panel {
      Panel::None => HEIGHT,
      Panel::Music if !self.signed_in => {
        SIGN_IN_Y
          + SIGN_IN_BUTTON_HEIGHT
          + 12
          + if self.playlist_error.is_some()
            && self.signing_in.is_none()
            && self.confirmation.is_none()
          {
            22
          } else {
            0
          }
      }
      Panel::Music if self.lyrics_view() => TRANSPORT_END + 16 + self.player_shift(),
      Panel::Music => MUSIC_BOTTOM,
      Panel::Audio => {
        let count = self.count().min(VISIBLE_ROWS) as i32;
        PANEL_Y
          + if count == 0 && self.audio_error.is_none() {
            106
          } else {
            APP_AUDIO_Y + count * ROW_HEIGHT
          }
      }
      Panel::History | Panel::Tray => {
        PANEL_Y + 42 + self.count().clamp(1, VISIBLE_ROWS) as i32 * ROW_HEIGHT
      }
    }
  }

  fn music_status(&self) -> Option<&str> {
    self
      .playlist_error
      .as_deref()
      .or(self.mic_error.as_deref())
      .or(self.mic_pending.then_some("Connecting music to mic…"))
      .or(self.audio_error.as_deref())
      .or(self.playlist_busy.then_some("Loading…"))
  }

  fn confirmation_geometry(&self) -> (i32, i32, i32) {
    if self.signed_in {
      (
        PLAYER_X + 6,
        PANEL_Y + SEEK_Y - 2 + self.player_shift(),
        PLAYER_WIDTH - 16,
      )
    } else {
      (SIGN_IN_X, SIGN_IN_Y - 1, SIGN_IN_WIDTH)
    }
  }

  pub fn visible_playlists(&self) -> impl Iterator<Item = &Playlist> {
    self
      .playlists
      .iter()
      .skip(self.list_offset())
      .take(PLAYLIST_ROWS + 1)
  }

  fn visible_queue(&self) -> impl Iterator<Item = (usize, &QueueSong)> {
    let first = self.track.queue_index + 1 + self.list_offset();
    let end = self.track.queue_songs.len().min(first + QUEUE_ROWS + 1);
    (first..end).map(|row| {
      let index = self
        .queue_drag
        .as_ref()
        .map_or(row, |drag| drag.source_index(row));
      (index, &self.track.queue_songs[index])
    })
  }

  fn playlist_targets(&self) -> impl Iterator<Item = &Playlist> {
    self
      .playlists
      .iter()
      .filter(|playlist| playlist.id.trim_start_matches("VL").starts_with("PL"))
  }

  fn visible_results(&self) -> impl Iterator<Item = &Song> {
    self
      .search_results
      .iter()
      .skip(self.list_offset())
      .take(SEARCH_ROWS + 1)
  }

  pub fn visible_cover_urls(&self) -> Vec<&str> {
    match self.music_tab {
      MusicTab::Playlists => self
        .visible_playlists()
        .map(|list| list.art.as_str())
        .collect(),
      MusicTab::Queue => self
        .visible_queue()
        .map(|(_, song)| song.art.as_str())
        .collect(),
      MusicTab::Search if self.playlist_target.is_some() => self
        .playlist_targets()
        .skip(self.list_offset())
        .take(SEARCH_ROWS + 1)
        .map(|list| list.art.as_str())
        .collect(),
      MusicTab::Search => self
        .visible_results()
        .map(|song| song.art.as_str())
        .collect(),
      MusicTab::Lyrics => Vec::new(),
    }
  }

  pub fn cover_url(&self) -> &str {
    self
      .starting_song
      .as_ref()
      .map_or(&self.track.art, |(song, _)| &song.art)
  }

  pub fn update_track(&mut self, track: Track) {
    if self.track.id != track.id {
      self.title_scroll.set(None);
      self.pending_seek = None;
    }
    if self.track.player_pid != track.player_pid {
      self.streams.retain(|stream| !stream.music);
    }
    if self
      .queue_drag
      .as_ref()
      .is_some_and(|drag| drag.expected != track.queue || drag.playing != track.queue_index)
    {
      self.queue_drag = None;
    }
    if self
      .starting_song
      .as_ref()
      .is_some_and(|(song, _)| song.id == track.id)
    {
      self.starting_song = None;
    }
    self.track = track;
  }

  pub fn cancel_queue_drag(&mut self) {
    self.queue_drag = None;
  }

  fn visible_list_rows(&self) -> usize {
    if self.panel == Panel::Music {
      match self.music_tab {
        MusicTab::Playlists => PLAYLIST_ROWS,
        MusicTab::Queue => QUEUE_ROWS,
        MusicTab::Search => SEARCH_ROWS,
        MusicTab::Lyrics => 1,
      }
    } else {
      VISIBLE_ROWS
    }
  }

  fn list_offset(&self) -> usize {
    self
      .offset
      .min(self.count().saturating_sub(self.visible_list_rows()))
  }

  pub fn can_remove_song(&self) -> bool {
    self.signed_in
      && !self.playlist_busy
      && self
        .selected_playlist
        .as_ref()
        .is_some_and(|list| list.songs.contains(&self.track.id))
  }

  fn sign_in_hit(&self, x: i32, y: i32) -> bool {
    let width = if self.reconnect {
      RECONNECT_WIDTH
    } else {
      SIGN_IN_WIDTH
    };
    (SIGN_IN_X..SIGN_IN_X + width).contains(&x)
      && (SIGN_IN_Y..SIGN_IN_Y + SIGN_IN_BUTTON_HEIGHT).contains(&y)
  }

  pub fn remember_playlist(&mut self) {
    if let Some(active) = &self.selected_playlist
      && active.songs.contains(&self.track.id)
    {
      self.playlist_bookmarks.insert(
        active.id.clone(),
        Bookmark {
          id: self.track.id.clone(),
          order: self.track.queue.clone(),
          position: self.track.elapsed(),
        },
      );
    }
  }

  pub fn reset_music_scroll(&mut self) {
    self.lyrics_scrolled = false;
    self.offset = 0;
  }

  pub fn clear_search_focus(&mut self) {
    self.search_row = None;
  }

  pub fn search_has_focus_row(&self) -> bool {
    self.music_tab == MusicTab::Search && self.search_row.is_some()
  }

  pub fn search_keyboard(&self) -> bool {
    self.panel == Panel::Music
      && self.signed_in
      && self.music_tab == MusicTab::Search
      && self.playlist_target.is_none()
      && self.confirmation.is_none()
  }

  pub fn search_edited(&mut self) {
    self.search_row = None;
    self.search_revision = self.search_revision.wrapping_add(1);
    self.search_results.clear();
    self.search_status = None;
    self.add_feedback = None;
    self.offset = 0;
    self.search_pending =
      (!self.search_query.trim().is_empty()).then(|| Instant::now() + Duration::from_millis(250));
  }

  pub fn music_tab_shortcut(&mut self, index: usize) -> Option<Action> {
    if !self.wide_music() || self.confirmation.is_some() {
      return None;
    }
    let tab = *MUSIC_TABS.get(index)?;
    self.music_tab = tab;
    self.row_scroll.set(None);
    self.playlist_target = None;
    self.search_row = None;
    self.reset_music_scroll();
    self.cancel_queue_drag();
    Some(Action::MusicTab(tab))
  }

  pub fn scroll_focused_panel(&mut self, down: bool) -> bool {
    if self.panel == Panel::None || self.confirmation.is_some() {
      return false;
    }
    let previous = (self.offset, self.search_row, self.lyrics_scrolled);
    if !self.move_search_focus(down) {
      self.scroll(if down { 1.0 } else { -1.0 }, MUSIC_LEFT, LYRICS_TOP);
    }
    previous != (self.offset, self.search_row, self.lyrics_scrolled)
  }

  pub fn follow_lyrics(&mut self) -> bool {
    if !self.lyrics_view() {
      return false;
    }
    self.reset_music_scroll();
    true
  }

  pub fn toggle_from_lyrics(&self) -> Option<Action> {
    (self.lyrics_view() && self.track.has_track && self.confirmation.is_none()).then_some(
      Action::Media {
        command: "toggle",
        rewind: self.track.at_end,
      },
    )
  }

  pub fn move_search_focus(&mut self, down: bool) -> bool {
    if !self.wide_music() || self.music_tab != MusicTab::Search || self.confirmation.is_some() {
      return false;
    }
    let count = self.count();
    self.search_row = (count > 0).then(|| {
      self.search_row.map_or(self.list_offset(), |row| {
        if down {
          (row + 1).min(count - 1)
        } else {
          row.saturating_sub(1)
        }
      })
    });
    if let Some(row) = self.search_row {
      self.offset = self
        .offset
        .clamp((row + 1).saturating_sub(SEARCH_ROWS), row);
    }
    true
  }

  pub fn activate_search_focus(&mut self, add: bool) -> Option<Action> {
    let row = self.search_row?;
    if self.music_tab != MusicTab::Search || !self.wide_music() || self.confirmation.is_some() {
      return None;
    }
    let y = RESULTS_Y + (row.checked_sub(self.list_offset())? as i32) * RESULTS_ROW + 12;
    let x = if add && self.playlist_target.is_none() {
      SAVE_X + 4
    } else {
      MUSIC_LEFT + 8
    };
    self.hit(Layer::Bar, x, y, false)
  }

  pub fn drag_music(&mut self, x: i32, y: i32) -> bool {
    let duration = self.track.duration;
    if let Some((_, position)) = &mut self.seek_preview {
      let next = seek_position(x, duration);
      return std::mem::replace(position, next) != next;
    }
    let offset = self.list_offset();
    if let Some(drag) = &mut self.queue_drag
      && !drag.submitted
    {
      let start = self.track.queue_index + 1;
      let end = self.track.queue.len().saturating_sub(1);
      if start <= end {
        let row = (y - LIST_Y)
          .div_euclid(QUEUE_ROW)
          .clamp(0, (TAB_END - LIST_Y - 1) / QUEUE_ROW);
        let next = (start + offset + row as usize).min(end);
        return std::mem::replace(&mut drag.to, next) != next;
      }
    }
    false
  }

  pub fn finish_music_drag(&mut self) -> Option<Action> {
    if let Some((track, seconds)) = self.seek_preview.take() {
      return (track == self.track.id).then_some(Action::Seek(seconds));
    }
    let drag = self.queue_drag.as_mut()?;
    if drag.submitted {
      return None;
    }
    if drag.from == drag.to || drag.expected != self.track.queue {
      self.queue_drag = None;
      return None;
    }
    let moved = drag.from.min(drag.to)..=drag.from.max(drag.to);
    if drag.expected[moved]
      .iter()
      .all(|id| id == &drag.expected[drag.from])
    {
      self.queue_drag = None;
      return None;
    }
    drag.submitted = true;
    Some(Action::Reorder {
      expected: drag.expected.clone(),
      from: drag.from,
      to: drag.to,
    })
  }

  pub fn hit(&mut self, layer: Layer, x: i32, y: i32, right: bool) -> Option<Action> {
    if layer == Layer::Popup {
      let stride = POPUP_HEIGHT + POPUP_GAP;
      let row = y.div_euclid(stride);
      let y = y.rem_euclid(stride);
      if row < 0 || y >= POPUP_HEIGHT {
        return None;
      }
      let notice = self.popups().nth(row as usize)?;
      if popup_close_hit(x, y) || right {
        return Some(Action::Close(notice.id));
      }
      for (index, (key, _)) in notice.popup_actions().enumerate() {
        let left = POPUP_PADDING + index as i32 * (POPUP_ACTION_WIDTH + 8);
        if inside(
          Some((x.into(), y.into())),
          left,
          POPUP_ACTION_Y,
          POPUP_ACTION_WIDTH,
          22,
        ) {
          return Some(Action::Invoke(notice.id, key.clone()));
        }
      }
      return notice.activate().or(Some(Action::Close(notice.id)));
    }
    if y >= self.panel_bottom() {
      return None;
    }
    if y < HEIGHT {
      if (HISTORY_X..TRAY_X).contains(&x) {
        if right {
          return Some(Action::ToggleDoNotDisturb);
        }
        self.select(if self.panel == Panel::History {
          Panel::None
        } else {
          Panel::History
        });
      } else if (TRAY_X..TRAY_X + UTILITY_WIDTH).contains(&x) {
        self.select(if self.panel == Panel::Tray {
          Panel::None
        } else {
          Panel::Tray
        });
      } else if (AUDIO_X..AUDIO_X + UTILITY_WIDTH).contains(&x) {
        let open = self.panel != Panel::Audio;
        self.select(if open { Panel::Audio } else { Panel::None });
        return open.then_some(Action::OpenAudio);
      } else if !right && (MUSIC_X..MUSIC_X + UTILITY_WIDTH).contains(&x) {
        let open = self.panel != Panel::Music;
        self.select(if open { Panel::Music } else { Panel::None });
        return open.then_some(Action::OpenMusic);
      }
      return None;
    }
    if self.panel == Panel::Audio {
      let pointer = Some((f64::from(x), f64::from(y)));
      if !right && monitor_hit(pointer) {
        return Some(Action::ToggleMonitor);
      }
      if let Some((_, device)) = AUDIO_DEVICES
        .into_iter()
        .enumerate()
        .find(|(index, _)| audio_button_hit(pointer, *index))
      {
        return Some(Action::Volume(Slider::Device(device), None));
      }
      if !(PANEL_X + 12..PANEL_X + PANEL_WIDTH - 12).contains(&x) {
        return None;
      }
      let y = y - PANEL_Y;
      return match y {
        34..=68 => Slider::Device(Device::Output).pick(x, right),
        69..=103 => Slider::Device(Device::Input).pick(x, right),
        APP_AUDIO_Y.. if right || x >= SLIDER_X => {
          let row = self.offset.min(self.count().saturating_sub(VISIBLE_ROWS))
            + ((y - APP_AUDIO_Y) / ROW_HEIGHT) as usize;
          self
            .app_streams()
            .nth(row)
            .and_then(|stream| Slider::Stream(stream.id).pick(x, right))
        }
        _ => None,
      };
    }
    if self.panel == Panel::Music {
      if self.confirmation.is_some() {
        let (left, top, width) = self.confirmation_geometry();
        if !right && (top + 21..top + 45).contains(&y) && (left..left + width).contains(&x) {
          let pending = self.confirmation.take();
          if x >= left + width / 2 + 3 {
            return pending.map(|pending| match pending {
              MusicConfirmation::Remove(removal) => Action::RemoveSong(removal),
              MusicConfirmation::SignOut => Action::SignOut,
            });
          }
        }
        return None;
      }
      if self.signed_in
        && !self.lyrics_view()
        && self.music_mix
        && !right
        && music_gain_icon_hit(Some((x.into(), y.into())), MusicChannel::Send)
      {
        return Some(Action::ToggleMic);
      }
      if self.signed_in
        && !self.lyrics_view()
        && !right
        && music_gain_icon_hit(Some((x.into(), y.into())), MusicChannel::Listen)
        && let Some((slider, _)) = self.music_slider(MusicChannel::Listen)
      {
        return Some(Action::Volume(slider, None));
      }
      if self.signed_in
        && !self.lyrics_view()
        && (MUSIC_GAIN_TOP - 6..=MUSIC_GAIN_BOTTOM + 6).contains(&y)
      {
        for channel in MusicChannel::ALL {
          if (x - music_gain_x(channel)).abs() <= 12
            && let Some((slider, _)) = self.music_slider(channel)
          {
            return Some(Action::Volume(
              slider,
              (!right).then(|| slider.position(x, y)),
            ));
          }
        }
      }
      let panel = if self.signed_in {
        MUSIC_PANEL_X..MUSIC_PANEL_X + MUSIC_PANEL_WIDTH
      } else {
        PANEL_X..PANEL_X + PANEL_WIDTH
      };
      if right || !panel.contains(&x) {
        return None;
      }
      if !self.signed_in {
        if !self.library_checked {
          return (!self.library_refreshing
            && self.playlist_error.is_some()
            && self.sign_in_hit(x, y))
          .then_some(Action::OpenMusic);
        }
        if self.reconnect
          && !self.library_refreshing
          && !self.playlist_busy
          && (SIGN_IN_X + RECONNECT_WIDTH + 8..SIGN_IN_X + SIGN_IN_WIDTH).contains(&x)
          && (SIGN_IN_Y..SIGN_IN_Y + SIGN_IN_BUTTON_HEIGHT).contains(&y)
        {
          self.confirmation = Some(MusicConfirmation::SignOut);
          return None;
        }
        return (!self.library_refreshing && !self.playlist_busy && self.sign_in_hit(x, y))
          .then_some(Action::SignIn);
      }
      if !self.playlist_busy
        && inside(
          Some((f64::from(x), f64::from(y))),
          PLAYER_SIGN_OUT_X,
          SIGN_OUT_Y,
          TAB_WIDTH,
          MUSIC_TAB_HEIGHT,
        )
      {
        self.confirmation = Some(MusicConfirmation::SignOut);
        return None;
      }
      if self.can_remove_song()
        && inside(
          Some((x.into(), y.into())),
          PLAYER_REMOVE_X,
          PANEL_Y + TRANSPORT_Y + self.player_shift(),
          TAB_WIDTH,
          27,
        )
      {
        self.confirmation = self.selected_playlist.as_ref().map(|playlist| {
          MusicConfirmation::Remove(Removal {
            playlist: playlist.id.clone(),
            song: self.track.id.clone(),
            title: self.track.title.clone(),
          })
        });
        return None;
      }
      let relative_y = y - PANEL_Y - self.player_shift();
      let controls = x - TRANSPORT_X;
      if (TRANSPORT_Y..TRANSPORT_Y + 29).contains(&relative_y)
        && (0..3 * BUTTON_WIDTH).contains(&controls)
        && self.track.has_track
      {
        let index = (controls / BUTTON_WIDTH) as usize;
        return Some(Action::Media {
          command: ["prev", "toggle", "next"][index],
          rewind: index == 1 && self.track.at_end,
        });
      }
      if (SEEK_Y - 4..SEEK_Y + 8).contains(&relative_y)
        && self.track.duration > 0.0
        && (PLAYER_X + 8..PLAYER_X + PLAYER_WIDTH - 10).contains(&x)
      {
        self.seek_preview = Some((self.track.id.clone(), seek_position(x, self.track.duration)));
        return None;
      }
      if (TAB_Y..TAB_Y + MUSIC_TAB_HEIGHT).contains(&y)
        && (TAB_START..TAB_START + 4 * MUSIC_TAB_WIDTH).contains(&x)
      {
        return self.music_tab_shortcut(((x - TAB_START) / MUSIC_TAB_WIDTH) as usize);
      }
      if self.lyrics_view() {
        if self.lyrics_scrolled
          && (WIDTH - LYRICS_LEFT - 76..WIDTH - LYRICS_LEFT).contains(&x)
          && (LIST_Y - 10..LYRICS_TOP).contains(&y)
        {
          self.lyrics_scrolled = false;
        } else if self.track.duration > 0.0
          && (LYRICS_LEFT..LYRICS_LEFT + LYRICS_WIDTH).contains(&x)
          && let Some((_, _, start)) = self
            .lyric_hits
            .borrow()
            .iter()
            .find(|(top, bottom, _)| (*top..*bottom).contains(&y))
        {
          self.lyrics_scrolled = false;
          return Some(Action::Seek(start.clamp(0.0, self.track.duration)));
        }
        return None;
      }
      if (MUSIC_LEFT - 4..PLAYER_X).contains(&x) && (LIST_Y..TAB_END).contains(&y) {
        match self.music_tab {
          MusicTab::Playlists if !self.playlist_busy => {
            let row = ((y - LIST_Y) / PLAYLIST_ROW) as usize;
            if row <= PLAYLIST_ROWS {
              return self
                .playlists
                .get(self.list_offset() + row)
                .filter(|playlist| {
                  self
                    .selected_playlist
                    .as_ref()
                    .is_none_or(|active| active.id != playlist.id)
                })
                .cloned()
                .map(Action::Play);
            }
          }
          MusicTab::Queue => {
            let row = ((y - LIST_Y) / QUEUE_ROW) as usize;
            let index = self.track.queue_index + 1 + self.list_offset() + row;
            if row <= QUEUE_ROWS
              && index < self.track.queue.len()
              && !self.queue_drag.as_ref().is_some_and(|drag| drag.submitted)
            {
              self.queue_drag = Some(QueueDrag {
                expected: self.track.queue.clone(),
                playing: self.track.queue_index,
                from: index,
                to: index,
                submitted: false,
              });
            }
          }
          MusicTab::Search if self.playlist_target.is_some() => {
            if y < LIST_Y + 32 {
              self.dismiss_confirmation();
              return Some(Action::SmallCovers);
            }
            if !self.adding_song && (RESULTS_Y..SEARCH_END).contains(&y) {
              let row = ((y - RESULTS_Y) / RESULTS_ROW) as usize;
              let playlist = self
                .playlist_targets()
                .nth(self.list_offset() + row)?
                .clone();
              let song = self.close_playlist_picker()?;
              return Some(Action::AddSong(playlist, song));
            }
          }
          MusicTab::Search if (RESULTS_Y..SEARCH_END).contains(&y) => {
            let row = ((y - RESULTS_Y) / RESULTS_ROW) as usize;
            let song = self.search_results.get(self.list_offset() + row)?.clone();
            if inside(
              Some((x.into(), y.into())),
              SAVE_X,
              RESULTS_Y + row as i32 * RESULTS_ROW + 6,
              SAVE_WIDTH,
              30,
            ) {
              if !self.adding_song {
                self.playlist_target = Some(PlaylistTarget {
                  song,
                  results_offset: self.offset,
                  search_row: self.search_row,
                });
                self.search_row = None;
                self.add_feedback = None;
                self.offset = 0;
                return Some(Action::SmallCovers);
              }
            } else if !self.playlist_busy {
              return Some(Action::PlayResult(song));
            }
          }
          MusicTab::Search if y < LIST_Y + 30 => {
            self.clear_search_focus();
            let query = self.search_query.trim();
            return (x >= PLAYER_X - 36 && !query.is_empty())
              .then(|| Action::Search(query.to_owned()));
          }
          _ => {}
        }
      }
      return None;
    }
    if self.panel == Panel::History
      && !right
      && !self.notices.is_empty()
      && inside(
        Some((x.into(), y.into())),
        CLEAR_X,
        PANEL_Y + 5,
        CLEAR_WIDTH,
        26,
      )
    {
      return Some(Action::ClearNotifications);
    }
    let row = y - PANEL_Y - 34;
    if row < 0 {
      return None;
    }
    let row = self.list_offset() + (row / ROW_HEIGHT) as usize;
    match self.panel {
      Panel::History => {
        let notice = self
          .notices
          .iter_mut()
          .filter(|notice| !notice.transient)
          .nth(row)?;
        let end = PANEL_X + PANEL_WIDTH - 36 + POPUP_CLOSE_SIZE;
        if right || (end - POPUP_CLOSE_SIZE..end).contains(&x) {
          Some(Action::Close(notice.id))
        } else {
          notice.seen = true;
          notice.activate()
        }
      }
      Panel::Tray => {
        let item = self.trays.get(row)?;
        Some(Action::Tray(
          item.address.clone(),
          if right || item.menu_only {
            "ContextMenu"
          } else {
            "Activate"
          },
        ))
      }
      _ => None,
    }
  }
}

const SLIDER_X: i32 = PANEL_X + 135;
const MONITOR_X: i32 = PANEL_X + PANEL_WIDTH - 40;
const CLEAR_WIDTH: i32 = 80;
const CLEAR_X: i32 = PANEL_X + PANEL_WIDTH - CLEAR_WIDTH - 12;
const POPUP_ACTION_Y: i32 = 83;
const POPUP_ACTION_WIDTH: i32 = 104;
const POPUP_PADDING: i32 = 14;

impl Notice {
  fn popup_text_x(&self) -> i32 {
    if self.image.is_some() {
      62
    } else {
      POPUP_PADDING
    }
  }

  fn popup_actions(&self) -> impl Iterator<Item = &(String, String)> {
    self
      .actions
      .iter()
      .filter(|(key, _)| key != "default")
      .take(2)
  }
}
fn monitor_hit(pointer: Option<(f64, f64)>) -> bool {
  inside(pointer, MONITOR_X, PANEL_Y + 5, 28, 28)
}
const SLIDER_END: i32 = PANEL_X + PANEL_WIDTH - 48;

fn audio_slider_y(index: usize) -> i32 {
  PANEL_Y + 43 + index as i32 * 35
}

fn music_gain_x(channel: MusicChannel) -> i32 {
  let inset = (PLAYER_WIDTH - COVER) / 4;
  match channel {
    MusicChannel::Listen => PLAYER_X + inset,
    MusicChannel::Send => PLAYER_X + PLAYER_WIDTH - inset,
  }
}

fn music_gain_icon_hit(pointer: Option<(f64, f64)>, channel: MusicChannel) -> bool {
  inside(
    pointer,
    music_gain_x(channel) - 13,
    MUSIC_GAIN_TOP - 30,
    26,
    26,
  )
}

fn audio_button_hit(pointer: Option<(f64, f64)>, index: usize) -> bool {
  inside(
    pointer,
    AUDIO_BUTTON_X,
    audio_slider_y(index) + 3 - AUDIO_BUTTON_SIZE / 2,
    AUDIO_BUTTON_SIZE,
    AUDIO_BUTTON_SIZE,
  )
}

fn popup_close_hit(x: i32, y: i32) -> bool {
  (POPUP_CLOSE_X..POPUP_CLOSE_X + POPUP_CLOSE_SIZE).contains(&x)
    && (POPUP_CLOSE_Y..POPUP_CLOSE_Y + POPUP_CLOSE_SIZE).contains(&y)
}

fn seek_position(x: i32, duration: f64) -> f64 {
  let width = PLAYER_WIDTH - 18;
  duration * f64::from((x - PLAYER_X - 8).clamp(0, width)) / f64::from(width)
}

fn sign_out_button(cr: &Context, x: i32, y: i32, height: i32) -> cairo::Result<()> {
  round(cr, x, y, SIGN_OUT_WIDTH, height, 7.0);
  color(cr, 0xcdd6f4, 0.09);
  cr.fill()?;
  let x = f64::from(x) + f64::from(SIGN_OUT_WIDTH) / 2.0;
  let y = f64::from(y) + f64::from(height) / 2.0;
  color(cr, 0xcdd6f4, 1.0);
  sign_out_symbol(cr, x, y)
}

fn sign_out_symbol(cr: &Context, x: f64, y: f64) -> cairo::Result<()> {
  cr.set_line_width(1.45);
  cr.move_to(x - 1.0, y - 6.0);
  cr.line_to(x - 6.0, y - 6.0);
  cr.line_to(x - 6.0, y + 6.0);
  cr.line_to(x - 1.0, y + 6.0);
  cr.move_to(x - 2.0, y);
  cr.line_to(x + 6.0, y);
  cr.move_to(x + 2.5, y - 3.5);
  cr.line_to(x + 6.0, y);
  cr.line_to(x + 2.5, y + 3.5);
  cr.stroke()
}

pub fn app_icon(name: &str) -> Option<ImageSurface> {
  if name.is_empty() || name.contains('/') || name == "." || name == ".." {
    return None;
  }
  let filename = format!("{name}.png");
  for root in gio::glib::system_data_dirs() {
    for size in [48, 32, 64, 128, 256, 24, 16, 512] {
      let path = root
        .join(format!("icons/hicolor/{size}x{size}/apps"))
        .join(&filename);
      if let Ok(bytes) = std::fs::read(path) {
        return crate::media::decode_art(bytes, 24).and_then(image);
      }
    }
    if let Ok(bytes) = std::fs::read(root.join("pixmaps").join(&filename)) {
      return crate::media::decode_art(bytes, 24).and_then(image);
    }
  }
  None
}

fn draw_volume(cr: &Context, y: i32, slider: Slider, volume: Option<&Volume>) -> cairo::Result<()> {
  let x = slider.start();
  let width = SLIDER_END - x;
  round(cr, x, y, width, 8, 4.0);
  color(cr, 0x45475a, 1.0);
  cr.fill()?;
  if let Some(volume) = volume {
    let length = width * i32::from(volume.percent.min(100)) / 100;
    color(cr, if volume.muted { 0x6c7086 } else { 0xcba6f7 }, 1.0);
    cr.rectangle(f64::from(x), f64::from(y + 2), f64::from(length), 4.0);
    cr.fill()?;
    color(cr, 0xcdd6f4, 1.0);
    let value = if volume.muted {
      "Muted".into()
    } else {
      format!("{}%", volume.percent)
    };
    label(
      cr,
      &value,
      (SLIDER_END + 3, y - 3, 37),
      11,
      Weight::Normal,
      Alignment::Right,
    );
  }
  Ok(())
}

pub fn image(art: Art) -> Option<ImageSurface> {
  ImageSurface::create_for_data(
    art.data,
    Format::ARgb32,
    art.width,
    art.height,
    art.width * 4,
  )
  .ok()
}

fn color(cr: &Context, rgb: u32, alpha: f64) {
  cr.set_source_rgba(
    f64::from(rgb >> 16 & 255) / 255.0,
    f64::from(rgb >> 8 & 255) / 255.0,
    f64::from(rgb & 255) / 255.0,
    alpha,
  );
}

pub(crate) fn round(cr: &Context, x: i32, y: i32, w: i32, h: i32, radius: f64) {
  let (x, y, w, h) = (f64::from(x), f64::from(y), f64::from(w), f64::from(h));
  let half_pi = std::f64::consts::FRAC_PI_2;
  cr.new_sub_path();
  cr.arc(x + w - radius, y + radius, radius, -half_pi, 0.0);
  cr.arc(x + w - radius, y + h - radius, radius, 0.0, half_pi);
  cr.arc(x + radius, y + h - radius, radius, half_pi, 2.0 * half_pi);
  cr.arc(x + radius, y + radius, radius, 2.0 * half_pi, 3.0 * half_pi);
  cr.close_path();
}

fn text_layout(cr: &Context, text: &str, size: i32, weight: Weight) -> pango::Layout {
  let layout = pangocairo::functions::create_layout(cr);
  let mut font = FontDescription::new();
  font.set_family("sans-serif");
  font.set_absolute_size(f64::from(size * SCALE));
  font.set_weight(weight);
  layout.set_font_description(Some(&font));
  layout.set_text(text);
  layout.set_single_paragraph_mode(true);
  layout
}

#[derive(Eq, Hash, PartialEq)]
struct LabelKey {
  text: String,
  size: i32,
  weight: Weight,
  width: i32,
  align: Alignment,
  scale: u64,
  lines: i32,
}

thread_local! {
  static LABEL_LAYOUTS: RefCell<HashMap<LabelKey, pango::Layout>> = RefCell::default();
}

fn label(
  cr: &Context,
  text: &str,
  rect: (i32, i32, i32),
  size: i32,
  weight: Weight,
  align: Alignment,
) {
  label_lines(cr, text, rect, size, weight, align, 1);
}

fn label_lines(
  cr: &Context,
  text: &str,
  rect: (i32, i32, i32),
  size: i32,
  weight: Weight,
  align: Alignment,
  lines: i32,
) {
  let (x, y, width) = rect;
  let create = || {
    let layout = text_layout(cr, text, size, weight);
    if width != 0 {
      layout.set_width(width * SCALE);
      layout.set_ellipsize(EllipsizeMode::End);
      layout.set_alignment(align);
      if lines > 1 {
        layout.set_wrap(pango::WrapMode::WordChar);
        layout.set_height(-lines);
      }
    }
    layout
  };
  let paint = |layout: &pango::Layout| {
    let mut x = f64::from(x);
    if width == 0 {
      x -= f64::from(layout.pixel_size().0) / 2.0;
    }
    cr.move_to(x, f64::from(y));
    pangocairo::functions::show_layout(cr, layout);
  };
  if text.len() > 1024 {
    paint(&create());
    return;
  }
  // Lintel paints with a uniform output scale and optional translation.
  let key = LabelKey {
    text: text.into(),
    size,
    weight,
    width,
    align,
    scale: cr.matrix().xx().to_bits(),
    lines,
  };
  LABEL_LAYOUTS.with_borrow_mut(|layouts| {
    if let Some(layout) = layouts.get(&key) {
      paint(layout);
    } else {
      if layouts.len() == 256 {
        layouts.clear();
      }
      let layout = create();
      paint(&layout);
      layouts.insert(key, layout);
    }
  });
}

fn picture(
  cr: &Context,
  picture: Option<&ImageSurface>,
  x: i32,
  y: i32,
  size: i32,
  symbol: &str,
) -> cairo::Result<()> {
  round(cr, x, y, size, size, 6.0);
  cr.save()?;
  cr.clip();
  if let Some(img) = picture {
    let scale = f64::from(size) / f64::from(img.width().min(img.height()));
    cr.translate(
      f64::from(x) + (f64::from(size) - f64::from(img.width()) * scale) / 2.0,
      f64::from(y) + (f64::from(size) - f64::from(img.height()) * scale) / 2.0,
    );
    cr.scale(scale, scale);
    cr.set_source_surface(img, 0.0, 0.0)?;
    cr.paint()?;
  } else {
    color(cr, 0x313244, 1.0);
    cr.paint()?;
    color(cr, 0xa6adc8, 1.0);
    let layout = text_layout(cr, symbol, 15, Weight::Normal);
    let (ink, _) = layout.pixel_extents();
    cr.move_to(
      f64::from(x - ink.x()) + f64::from(size - ink.width()) / 2.0,
      f64::from(y - ink.y()) + f64::from(size - ink.height()) / 2.0,
    );
    pangocairo::functions::show_layout(cr, &layout);
  }
  cr.restore()
}

fn inside(pointer: Option<(f64, f64)>, x: i32, y: i32, w: i32, h: i32) -> bool {
  pointer.is_some_and(|(px, py)| {
    (f64::from(x)..f64::from(x + w)).contains(&px) && (f64::from(y)..f64::from(y + h)).contains(&py)
  })
}

fn dismissal(cr: &Context, x: i32, y: i32, hovered: bool) -> cairo::Result<()> {
  if hovered {
    round(cr, x, y, POPUP_CLOSE_SIZE, POPUP_CLOSE_SIZE, 6.0);
    color(cr, 0xf38ba8, 0.20);
    cr.fill()?;
  }
  color(cr, if hovered { 0xf38ba8 } else { 0xcdd6f4 }, 1.0);
  let x = f64::from(x + POPUP_CLOSE_SIZE / 2);
  let y = f64::from(y + POPUP_CLOSE_SIZE / 2);
  cr.set_line_width(1.5);
  cr.move_to(x - 4.0, y - 4.0);
  cr.line_to(x + 4.0, y + 4.0);
  cr.move_to(x - 4.0, y + 4.0);
  cr.line_to(x + 4.0, y - 4.0);
  cr.stroke()
}

fn clock_text(seconds: f64) -> String {
  let seconds = seconds as u64;
  if seconds >= 3600 {
    format!(
      "{}:{:02}:{:02}",
      seconds / 3600,
      seconds / 60 % 60,
      seconds % 60
    )
  } else {
    format!("{}:{:02}", seconds / 60, seconds % 60)
  }
}

fn paint_timeline(cr: &Context, model: &Model) -> cairo::Result<()> {
  let x = PLAYER_X + 8;
  let width = PLAYER_WIDTH - 18;
  let y = PANEL_Y + SEEK_Y + model.player_shift();
  let (position, duration) = if let Some((song, seconds)) = &model.starting_song {
    (*seconds, f64::from(song.seconds))
  } else {
    (model.playback_position(), model.track.duration)
  };
  color(cr, 0xa6adc8, 1.0);
  label(
    cr,
    &clock_text(position),
    (x, y - 18, width),
    11,
    Weight::Normal,
    Alignment::Left,
  );
  label(
    cr,
    &clock_text(duration),
    (x, y - 18, width),
    11,
    Weight::Normal,
    Alignment::Right,
  );
  color(cr, 0x45475a, 1.0);
  round(cr, x, y, width, 4, 2.0);
  cr.fill()?;
  if duration > 0.0 {
    let filled = (f64::from(width) * (position / duration).clamp(0.0, 1.0)).round() as i32;
    if filled > 0 {
      let hovered = inside(model.bar_pointer, x, y - 4, width, 12);
      color(cr, if hovered { 0xf5c2e7 } else { 0xcba6f7 }, 1.0);
      round(cr, x, y, filled, 4, 2.0_f64.min(f64::from(filled) / 2.0));
      cr.fill()?;
    }
  }
  Ok(())
}

fn transport(cr: &Context, model: &Model) -> cairo::Result<()> {
  let track = &model.track;
  for index in 0..3 {
    let x = TRANSPORT_X + index * BUTTON_WIDTH;
    if track.has_track
      && inside(
        model.bar_pointer,
        x,
        PANEL_Y + TRANSPORT_Y + model.player_shift(),
        BUTTON_WIDTH,
        29,
      )
    {
      round(cr, x + 2, 6, BUTTON_WIDTH - 4, 28, 6.0);
      color(cr, 0xcba6f7, 0.20);
      cr.fill()?;
    }
  }
  color(cr, 0xcdd6f4, if track.has_track { 1.0 } else { 0.4 });
  for index in 0..3 {
    let x = f64::from(TRANSPORT_X + BUTTON_WIDTH / 2 + index * BUTTON_WIDTH);
    if index == 1 && track.playing {
      cr.rectangle(x - 4.0, 15.0, 3.0, 10.0);
      cr.rectangle(x + 1.0, 15.0, 3.0, 10.0);
    } else {
      let direction = if index == 0 { -1.0 } else { 1.0 };
      cr.move_to(x - direction * 4.0, 15.0);
      cr.line_to(x + direction * 4.0, 20.0);
      cr.line_to(x - direction * 4.0, 25.0);
      cr.close_path();
      if index != 1 {
        cr.rectangle(x + direction * 5.0 - 0.75, 15.0, 1.5, 10.0);
      }
    }
    cr.fill()?;
  }
  if model.can_remove_song()
    && inside(
      model.bar_pointer,
      PLAYER_REMOVE_X,
      PANEL_Y + TRANSPORT_Y + model.player_shift(),
      TAB_WIDTH,
      27,
    )
  {
    round(cr, PLAYER_REMOVE_X, 5, TAB_WIDTH, 27, 6.0);
    color(cr, 0xf38ba8, 0.20);
    cr.fill()?;
  }
  let trash = f64::from(PLAYER_REMOVE_X + TAB_WIDTH / 2);
  color(
    cr,
    if model.can_remove_song() {
      0xf38ba8
    } else {
      0x7f849c
    },
    1.0,
  );
  cr.set_line_width(1.5);
  cr.move_to(trash - 6.0, 16.0);
  cr.line_to(trash + 6.0, 16.0);
  cr.move_to(trash - 2.0, 14.0);
  cr.line_to(trash + 2.0, 14.0);
  cr.move_to(trash - 4.5, 18.0);
  cr.line_to(trash - 3.5, 25.0);
  cr.line_to(trash + 3.5, 25.0);
  cr.line_to(trash + 4.5, 18.0);
  cr.stroke()?;

  Ok(())
}

fn edge_mask(
  first: f64,
  last: f64,
  leading: bool,
  trailing: bool,
  horizontal: bool,
) -> cairo::LinearGradient {
  let mask = if horizontal {
    cairo::LinearGradient::new(first, 0.0, last, 0.0)
  } else {
    cairo::LinearGradient::new(0.0, first, 0.0, last)
  };
  let fade = if horizontal { 16.0 } else { 48.0 };
  mask.add_color_stop_rgba(0.0, 0.0, 0.0, 0.0, if leading { 0.0 } else { 1.0 });
  mask.add_color_stop_rgba((fade / (last - first)).min(0.5), 0.0, 0.0, 0.0, 1.0);
  mask.add_color_stop_rgba(1.0 - (fade / (last - first)).min(0.5), 0.0, 0.0, 0.0, 1.0);
  mask.add_color_stop_rgba(1.0, 0.0, 0.0, 0.0, if trailing { 0.0 } else { 1.0 });
  mask
}

fn marquee(
  cr: &Context,
  text: &str,
  rect: (i32, i32, i32),
  size: i32,
  weight: Weight,
  centered: bool,
  started: Option<Instant>,
) -> cairo::Result<bool> {
  let (x, y, width) = rect;
  let layout = LABEL_LAYOUTS.with_borrow_mut(|layouts| {
    let key = LabelKey {
      text: text.to_owned(),
      size,
      weight,
      width: -1,
      align: Alignment::Left,
      scale: cr.matrix().xx().to_bits(),
      lines: 1,
    };
    if layouts.len() >= 256 {
      layouts.clear();
    }
    layouts
      .entry(key)
      .or_insert_with(|| text_layout(cr, text, size, weight))
      .clone()
  });
  let (measured, height) = layout.pixel_size();
  if measured <= width {
    let x = if centered {
      x + (width - measured) / 2
    } else {
      x
    };
    cr.move_to(f64::from(x), f64::from(y));
    pangocairo::functions::show_layout(cr, &layout);
    return Ok(false);
  }
  let period = f64::from(measured + 36);
  let offset = started.map_or(0.0, |time| {
    let phase = time.elapsed().as_secs_f64() % (1.0 + period / 42.0);
    ((phase - 1.0) * 42.0).max(0.0)
  });
  cr.save()?;
  cr.rectangle(
    f64::from(x),
    f64::from(y),
    f64::from(width),
    f64::from(height + 2),
  );
  cr.clip();
  cr.push_group();
  cr.move_to(f64::from(x) - offset, f64::from(y));
  pangocairo::functions::show_layout(cr, &layout);
  if offset + f64::from(width) > period {
    cr.move_to(f64::from(x) + period - offset, f64::from(y));
    pangocairo::functions::show_layout(cr, &layout);
  }
  cr.pop_group_to_source()?;
  cr.mask(&edge_mask(
    f64::from(x),
    f64::from(x + width),
    offset > 0.1,
    true,
    true,
  ))?;
  cr.restore()?;
  Ok(started.is_some())
}

fn row_title(
  cr: &Context,
  model: &Model,
  title: &str,
  rect: (i32, i32, i32),
  row_top: i32,
  row_height: i32,
  weight: Weight,
) -> cairo::Result<()> {
  let hovered = inside(
    model.bar_pointer,
    MUSIC_LEFT - 4,
    row_top,
    PLAYER_X - MUSIC_LEFT,
    row_height,
  );
  let started = hovered.then(|| {
    let key = model.list_offset() + ((row_top - LIST_Y).max(0) as usize);
    let start = model
      .row_scroll
      .get()
      .filter(|(row, _)| *row == key)
      .map_or_else(Instant::now, |(_, start)| start);
    model.row_scroll.set(Some((key, start)));
    start
  });
  if marquee(cr, title, rect, 14, weight, false, started)? {
    model.row_animating.set(true);
  }
  Ok(())
}

fn player_title(cr: &Context, model: &Model) -> cairo::Result<()> {
  let title = model
    .starting_song
    .as_ref()
    .map_or(model.track.title.as_str(), |(song, _)| song.title.as_str());
  let rect = if model.lyrics_view() {
    (
      MUSIC_LEFT + 72,
      LYRICS_FOOTER_Y + 17,
      PLAYER_X - MUSIC_LEFT - 86,
    )
  } else {
    (PLAYER_X + 12, PANEL_Y + 222, PLAYER_WIDTH - 24)
  };
  let start = model.title_scroll.get().unwrap_or_else(Instant::now);
  model.title_scroll.set(Some(start));
  color(cr, 0xcdd6f4, 1.0);
  model.title_animating.set(marquee(
    cr,
    if title.is_empty() {
      "Nothing playing"
    } else {
      title
    },
    rect,
    15,
    Weight::Semibold,
    !model.lyrics_view(),
    Some(start),
  )?);
  Ok(())
}

fn search_symbol(cr: &Context, x: i32, y: i32) -> cairo::Result<()> {
  let (x, y) = (f64::from(x), f64::from(y));
  cr.set_line_width(1.45);
  cr.new_sub_path();
  cr.arc(x - 1.5, y - 1.5, 4.4, 0.0, std::f64::consts::TAU);
  cr.move_to(x + 2.0, y + 2.0);
  cr.line_to(x + 6.5, y + 6.5);
  cr.stroke()
}

fn paint_search_field(cr: &Context, model: &Model, left: i32, width: i32) -> cairo::Result<()> {
  round(cr, left - 3, LIST_Y, width + 3, 30, 6.0);
  color(cr, 0x313244, 1.0);
  cr.fill()?;
  color(cr, 0xcdd6f4, 1.0);
  let query = if model.search_query.is_empty() {
    "Search songs"
  } else {
    &model.search_query
  };
  let layout = text_layout(cr, query, 12, Weight::Normal);
  let x = f64::from(left + 3);
  let y = f64::from(LIST_Y + 6);
  let field_width = f64::from(width - 33);
  let (caret, _) = layout.cursor_pos(model.search_cursor as i32);
  let caret_x = f64::from(caret.x()) / f64::from(SCALE);
  let scroll = (caret_x - field_width + 2.0).max(0.0);
  cr.save()?;
  cr.rectangle(x, y, field_width, 20.0);
  cr.clip();
  color(
    cr,
    if model.search_query.is_empty() {
      0xa6adc8
    } else {
      0xcdd6f4
    },
    1.0,
  );
  cr.move_to(x - scroll, y);
  pangocairo::functions::show_layout(cr, &layout);
  if model.search_cursor_visible {
    color(cr, 0xcba6f7, 1.0);
    cr.rectangle(x + caret_x - scroll, y + 1.0, 1.0, 15.0);
    cr.fill()?;
  }
  cr.restore()?;
  color(cr, 0xcdd6f4, 1.0);
  search_symbol(cr, PLAYER_X - 20, LIST_Y + 15)?;
  Ok(())
}

fn paint_music_library(cr: &Context, model: &Model, left: i32) -> cairo::Result<()> {
  let width = PLAYER_X - left - 8;
  for (index, name) in ["Playlists", "Queue", "Lyrics", "Search"]
    .into_iter()
    .enumerate()
  {
    let x = TAB_START + index as i32 * MUSIC_TAB_WIDTH;
    let selected = model.music_tab == MUSIC_TABS[index];
    let hovered = inside(
      model.bar_pointer,
      x,
      TAB_Y,
      MUSIC_TAB_WIDTH,
      MUSIC_TAB_HEIGHT,
    );
    if selected || hovered {
      round(cr, x + 1, TAB_Y, MUSIC_TAB_WIDTH - 3, MUSIC_TAB_HEIGHT, 7.0);
      color(cr, 0xcba6f7, if hovered { 0.25 } else { 0.16 });
      cr.fill()?;
    }
    color(cr, if selected { 0xcba6f7 } else { 0xcdd6f4 }, 1.0);
    label(
      cr,
      name,
      (x + 4, TAB_Y + 7, MUSIC_TAB_WIDTH - 8),
      13,
      Weight::Semibold,
      Alignment::Center,
    );
  }

  if !model.playlist_busy
    && inside(
      model.bar_pointer,
      PLAYER_SIGN_OUT_X,
      SIGN_OUT_Y,
      TAB_WIDTH,
      MUSIC_TAB_HEIGHT,
    )
  {
    round(
      cr,
      PLAYER_SIGN_OUT_X,
      SIGN_OUT_Y,
      TAB_WIDTH,
      MUSIC_TAB_HEIGHT,
      7.0,
    );
    color(cr, 0xcba6f7, 0.16);
    cr.fill()?;
  }
  color(cr, 0xa6adc8, if model.playlist_busy { 0.5 } else { 1.0 });
  sign_out_symbol(
    cr,
    f64::from(PLAYER_SIGN_OUT_X + TAB_WIDTH / 2),
    f64::from(SIGN_OUT_Y + MUSIC_TAB_HEIGHT / 2),
  )?;
  if model.lyrics_view() {
    return paint_lyrics(cr, model);
  }
  let bottom = if model.music_tab == MusicTab::Search {
    SEARCH_END
  } else {
    TAB_END
  };
  let fade_top = model.list_offset() > 0;
  let fade_bottom = model.count() > model.list_offset() + model.visible_list_rows();
  let fade = fade_top || fade_bottom;
  let top = if model.music_tab == MusicTab::Search {
    RESULTS_Y
  } else {
    LIST_Y
  };
  model.row_animating.set(false);
  cr.save()?;
  cr.rectangle(
    f64::from(left - 4),
    f64::from(LIST_Y),
    f64::from(PLAYER_X - left + 4),
    f64::from(bottom - LIST_Y),
  );
  cr.clip();
  if fade {
    cr.push_group();
  }
  match model.music_tab {
    MusicTab::Playlists => {
      if model.playlists.is_empty() && !model.playlist_busy {
        color(cr, 0xa6adc8, 1.0);
        label(
          cr,
          "No saved playlists",
          (left, LIST_Y + 7, width),
          12,
          Weight::Normal,
          Alignment::Left,
        );
      }
      for (row, list) in model.visible_playlists().enumerate() {
        let y = LIST_Y + row as i32 * PLAYLIST_ROW;
        let selected = model.loading_playlist.as_ref().map_or_else(
          || {
            model
              .selected_playlist
              .as_ref()
              .is_some_and(|active| active.id == list.id)
          },
          |loading| loading == &list.id,
        );
        let hovered = inside(model.bar_pointer, left - 4, y, width + 4, PLAYLIST_ROW - 3);
        if selected || hovered {
          round(cr, left - 4, y, width + 4, PLAYLIST_ROW - 3, 7.0);
          color(cr, 0xcba6f7, if hovered { 0.23 } else { 0.16 });
          cr.fill()?;
        }
        let cover = model.small_covers.get(&list.art).and_then(Option::as_ref);
        picture(cr, cover, left, y + 3, 30, "♪")?;
        color(cr, if selected { 0xcba6f7 } else { 0xcdd6f4 }, 1.0);
        row_title(
          cr,
          model,
          &list.name,
          (left + 36, y + 9, width - 36),
          y,
          PLAYLIST_ROW,
          Weight::Normal,
        )?;
      }
    }
    MusicTab::Queue => {
      if model.count() == 0 {
        color(cr, 0xa6adc8, 1.0);
        label(
          cr,
          "Queue empty",
          (left, LIST_Y + 7, width),
          12,
          Weight::Normal,
          Alignment::Left,
        );
      }
      for (row, (index, song)) in model.visible_queue().enumerate() {
        let y = LIST_Y + row as i32 * QUEUE_ROW;
        let dragged = model
          .queue_drag
          .as_ref()
          .is_some_and(|drag| drag.from == index);
        if dragged || inside(model.bar_pointer, left - 4, y, width + 4, QUEUE_ROW - 2) {
          round(cr, left - 4, y, width + 4, QUEUE_ROW - 2, 6.0);
          color(cr, 0xcba6f7, if dragged { 0.29 } else { 0.17 });
          cr.fill()?;
        }
        let cover = model.small_covers.get(&song.art).and_then(Option::as_ref);
        picture(cr, cover, left, y + 2, 26, "♪")?;
        color(cr, 0xcdd6f4, 1.0);
        row_title(
          cr,
          model,
          &song.title,
          (left + 32, y + 7, width - 32),
          y,
          QUEUE_ROW,
          Weight::Normal,
        )?;
      }
    }
    MusicTab::Lyrics => unreachable!(),
    MusicTab::Search if let Some(target) = &model.playlist_target => {
      let hovered = inside(model.bar_pointer, left - 4, LIST_Y, width + 4, 32);
      round(cr, left - 4, LIST_Y, width + 4, 32, 7.0);
      color(cr, 0xcba6f7, if hovered { 0.23 } else { 0.14 });
      cr.fill()?;
      color(cr, 0xcdd6f4, 1.0);
      label(
        cr,
        "‹   Add to playlist",
        (left + 4, LIST_Y + 1, width - 12),
        13,
        Weight::Semibold,
        Alignment::Left,
      );
      color(cr, 0xa6adc8, 1.0);
      label(
        cr,
        &target.song.title,
        (left + 20, LIST_Y + 17, width - 27),
        11,
        Weight::Normal,
        Alignment::Left,
      );
      for (row, list) in model
        .playlist_targets()
        .skip(model.list_offset())
        .take(SEARCH_ROWS + 1)
        .enumerate()
      {
        let y = RESULTS_Y + row as i32 * RESULTS_ROW;
        if model.search_row == Some(model.list_offset() + row)
          || inside(model.bar_pointer, left - 4, y, width + 4, RESULTS_ROW - 2)
        {
          round(cr, left - 4, y, width + 4, RESULTS_ROW - 2, 6.0);
          color(cr, 0xcba6f7, 0.17);
          cr.fill()?;
        }
        picture(
          cr,
          model.small_covers.get(&list.art).and_then(Option::as_ref),
          left,
          y + 3,
          28,
          "♪",
        )?;
        color(cr, 0xcdd6f4, 1.0);
        label(
          cr,
          &list.name,
          (left + 38, y + 7, width - 38),
          14,
          Weight::Normal,
          Alignment::Left,
        );
      }
      if model.count() == 0 {
        color(cr, 0xa6adc8, 1.0);
        label(
          cr,
          "No saved playlists to add to",
          (left, RESULTS_Y + 7, width),
          14,
          Weight::Normal,
          Alignment::Left,
        );
      }
    }
    MusicTab::Search => {
      paint_search_field(cr, model, left, width)?;
      if let Some(status) = &model.search_status {
        color(cr, 0xa6adc8, 1.0);
        label(
          cr,
          status,
          (left, RESULTS_Y, width),
          11,
          Weight::Normal,
          Alignment::Left,
        );
      }
      for (row, song) in model.visible_results().enumerate() {
        let y = RESULTS_Y + row as i32 * RESULTS_ROW;
        if model.search_row == Some(model.list_offset() + row)
          || inside(model.bar_pointer, left - 4, y, width + 4, RESULTS_ROW - 2)
        {
          round(cr, left - 4, y, width + 4, RESULTS_ROW - 2, 6.0);
          color(cr, 0xcba6f7, 0.17);
          cr.fill()?;
        }
        let cover = model.small_covers.get(&song.art).and_then(Option::as_ref);
        picture(cr, cover, left, y + 5, 32, "♪")?;
        color(cr, 0xcdd6f4, 1.0);
        row_title(
          cr,
          model,
          &song.title,
          (left + 39, y + 4, SAVE_X - left - 46),
          y,
          RESULTS_ROW,
          Weight::Semibold,
        )?;
        color(cr, 0xa6adc8, 1.0);
        label(
          cr,
          &song.artist,
          (left + 39, y + 23, SAVE_X - left - 46),
          11,
          Weight::Normal,
          Alignment::Left,
        );
        let hovered = inside(model.bar_pointer, SAVE_X, y + 6, SAVE_WIDTH, 30);
        let alpha = if model.adding_song {
          0.08
        } else if hovered {
          0.26
        } else {
          0.13
        };
        color(cr, 0xcba6f7, alpha);
        round(cr, SAVE_X, y + 6, SAVE_WIDTH, 30, 7.0);
        cr.fill()?;
        color(
          cr,
          if model.adding_song {
            0xa6adc8
          } else {
            0xcba6f7
          },
          1.0,
        );
        label(
          cr,
          "+ Add",
          (SAVE_X, y + 12, SAVE_WIDTH),
          12,
          Weight::Semibold,
          Alignment::Center,
        );
      }
    }
  }
  if fade {
    cr.pop_group_to_source()?;
    if top != LIST_Y {
      cr.save()?;
      cr.rectangle(
        f64::from(left - 4),
        f64::from(LIST_Y),
        f64::from(PLAYER_X - left + 4),
        f64::from(top - LIST_Y),
      );
      cr.clip();
      cr.paint()?;
      cr.restore()?;
    }
    cr.rectangle(
      f64::from(left - 4),
      f64::from(top),
      f64::from(PLAYER_X - left + 4),
      f64::from(bottom - top),
    );
    cr.clip();
    cr.mask(&edge_mask(
      f64::from(top),
      f64::from(bottom),
      fade_top,
      fade_bottom,
      false,
    ))?;
  }
  cr.restore()?;
  if !model.row_animating.get() {
    model.row_scroll.set(None);
  }
  if model.music_tab == MusicTab::Search && model.playlist_target.is_none() {
    if let Some(feedback) = model.add_feedback.as_deref().or_else(|| {
      model
        .search_row
        .map(|_| "↑↓ Select   Enter Play   Ctrl+Enter Add")
    }) {
      color(cr, 0xcba6f7, 1.0);
      label(
        cr,
        feedback,
        (left, SEARCH_END + 4, width),
        12,
        Weight::Semibold,
        Alignment::Left,
      );
    }
  }
  Ok(())
}

thread_local! {
  static LYRIC_LAYOUTS: RefCell<HashMap<(String, u64), pango::Layout>> = RefCell::default();
}

fn lyric_layout(cr: &Context, text: &str) -> pango::Layout {
  LYRIC_LAYOUTS.with_borrow_mut(|cache| {
    let key = (text.to_owned(), cr.matrix().xx().to_bits());
    if let Some(layout) = cache.get(&key) {
      return layout.clone();
    }
    if cache.len() >= 24 {
      cache.clear();
    }
    let layout = text_layout(cr, text, 29, Weight::Semibold);
    layout.set_width(LYRICS_WIDTH * SCALE);
    layout.set_wrap(pango::WrapMode::WordChar);
    cache.insert(key, layout.clone());
    layout
  })
}

fn paint_lyrics(cr: &Context, model: &Model) -> cairo::Result<()> {
  let mut hits = model.lyric_hits.borrow_mut();
  hits.clear();
  if let Some(status) = &model.lyrics_status {
    color(cr, 0xa6adc8, 1.0);
    label(
      cr,
      status,
      (LYRICS_LEFT, LYRICS_TOP + 30, LYRICS_WIDTH),
      19,
      Weight::Normal,
      Alignment::Left,
    );
    return Ok(());
  }
  let marker = model.lyric_marker();
  let current = marker.map(|marker| marker.index);
  let follow = current.is_some() && !model.lyrics_scrolled;
  let (offset, mut y) = if let Some(marker) = marker.filter(|_| follow) {
    let anchor = LYRICS_TOP + 81;
    if marker.index == 0 {
      (0, anchor)
    } else {
      let height = |index: usize| {
        lyric_layout(cr, &model.lyrics.lines[index].text)
          .pixel_size()
          .1
          + 20
      };
      let previous = marker.index - 1;
      let previous_height = height(previous);
      let t = f64::from(marker.scroll_progress) / 1024.0;
      let remaining = 1.0 - t * t * (3.0 - 2.0 * t);
      let y = anchor - previous_height + (remaining * f64::from(previous_height)).round() as i32;
      if previous > 0 && marker.scroll_progress < 1024 {
        (previous - 1, y - height(previous - 1))
      } else {
        (previous, y)
      }
    }
  } else {
    (model.offset, LYRICS_TOP)
  };
  if current.is_some() && !follow {
    let x = WIDTH - LYRICS_LEFT - 76;
    let hovered = inside(model.bar_pointer, x, LIST_Y - 10, 76, 28);
    round(cr, x, LIST_Y - 10, 76, 28, 7.0);
    color(cr, 0xcba6f7, if hovered { 0.24 } else { 0.13 });
    cr.fill()?;
    color(cr, 0xcba6f7, 1.0);
    label(
      cr,
      "Follow",
      (x, LIST_Y - 5, 76),
      13,
      Weight::Semibold,
      Alignment::Center,
    );
  }
  cr.save()?;
  cr.rectangle(
    f64::from(LYRICS_LEFT),
    f64::from(LYRICS_TOP),
    f64::from(LYRICS_WIDTH),
    f64::from(LYRICS_BOTTOM - LYRICS_TOP),
  );
  cr.clip();
  cr.push_group();
  for (index, line) in model.lyrics.lines.iter().enumerate().skip(offset) {
    if y >= LYRICS_BOTTOM {
      break;
    }
    let active = marker.filter(|marker| marker.index == index && marker.singing);
    let layout = lyric_layout(cr, &line.text);
    let attrs = active.filter(|marker| marker.word_end > 0).map(|marker| {
      let end = marker.word_end;
      let progress = marker.word_progress;
      let attrs = pango::AttrList::new();
      let previous = line
        .words
        .iter()
        .take_while(|word| word.end_byte < end)
        .last()
        .map_or(0, |word| word.end_byte);
      let mut sung = pango::AttrColor::new_foreground(0xffff, 0xf9f9, 0xf8f8);
      sung.set_end_index(previous);
      attrs.insert(sung);
      let tint =
        |from: u16, to: u16| from + ((u32::from(to - from) * u32::from(progress)) / 16) as u16;
      let mut word = pango::AttrColor::new_foreground(
        tint(0xacac, 0xffff),
        tint(0xaaaa, 0xf9f9),
        tint(0xc0c0, 0xf8f8),
      );
      word.set_start_index(previous);
      word.set_end_index(end);
      attrs.insert(word);
      attrs
    });
    layout.set_attributes(attrs.as_ref());
    let height = layout.pixel_size().1 + 20;
    let hovered = current.is_some()
      && inside(
        model.bar_pointer,
        LYRICS_LEFT,
        y.max(LYRICS_TOP),
        LYRICS_WIDTH,
        (y + height).min(LYRICS_BOTTOM) - y.max(LYRICS_TOP),
      );
    let (ink, alpha) = if active.is_some() {
      (
        if line.words.is_empty() {
          0xfff9f8
        } else {
          0xacaac0
        },
        1.0,
      )
    } else if hovered || !follow {
      (0xcdd6f4, 1.0)
    } else {
      let distance = current.unwrap_or(index).abs_diff(index).min(5) as f64;
      (0xcdd6f4, 0.68 - distance * 0.044)
    };
    color(cr, ink, alpha);
    cr.move_to(f64::from(LYRICS_LEFT), f64::from(y));
    pangocairo::functions::show_layout(cr, &layout);
    if let Some(start) = line.start
      && y + height > LYRICS_TOP
    {
      hits.push((y.max(LYRICS_TOP), (y + height).min(LYRICS_BOTTOM), start));
    }
    y += height;
  }
  cr.pop_group_to_source()?;
  cr.mask(&edge_mask(
    f64::from(LYRICS_TOP),
    f64::from(LYRICS_BOTTOM),
    true,
    true,
    false,
  ))?;
  cr.restore()?;
  Ok(())
}

fn paint_speaker(cr: &Context, x: f64, y: f64) -> cairo::Result<()> {
  cr.set_line_width(1.4);
  cr.move_to(x - 7.0, y - 3.0);
  cr.line_to(x - 3.0, y - 3.0);
  cr.line_to(x + 1.0, y - 7.0);
  cr.line_to(x + 1.0, y + 7.0);
  cr.line_to(x - 3.0, y + 3.0);
  cr.line_to(x - 7.0, y + 3.0);
  cr.close_path();
  cr.fill()?;
  cr.arc(x + 1.0, y, 6.0, -0.7, 0.7);
  cr.stroke()
}

fn paint_audio_device(cr: &Context, x: f64, y: f64, device: Device) -> cairo::Result<()> {
  cr.set_line_width(1.4);
  if device == Device::Input {
    round(cr, x as i32 - 3, y as i32 - 7, 6, 10, 3.0);
    cr.stroke()?;
    cr.arc(x, y + 1.0, 5.0, 0.0, std::f64::consts::PI);
    cr.move_to(x, y + 6.0);
    cr.rel_line_to(0.0, 3.0);
    cr.stroke()
  } else {
    cr.new_sub_path();
    cr.arc(x, y, 6.0, std::f64::consts::PI, std::f64::consts::TAU);
    cr.stroke()?;
    round(cr, x as i32 - 7, y as i32 - 1, 4, 7, 1.5);
    round(cr, x as i32 + 3, y as i32 - 1, 4, 7, 1.5);
    cr.fill()
  }
}

fn paint_music_gain(cr: &Context, model: &Model, channel: MusicChannel) -> cairo::Result<()> {
  let x = music_gain_x(channel);
  let icon_y = MUSIC_GAIN_TOP - 17;
  let level = model.music_slider(channel);
  if level.is_none() && (channel == MusicChannel::Listen || !model.music_mix) {
    return Ok(());
  }
  let device = match channel {
    MusicChannel::Listen => Device::Output,
    MusicChannel::Send => Device::Input,
  };
  let send = channel == MusicChannel::Send;
  let hovered = model.audio_hover[channel as usize].opacity();
  let gain_muted = level.is_some_and(|(_, volume)| volume.muted);
  let muted = gain_muted || (send && !model.mic_route);
  if hovered > 0.0 {
    round(cr, x - 12, icon_y - 12, 24, 24, 6.0);
    color(
      cr,
      if muted && !send { 0xf38ba8 } else { 0xcdd6f4 },
      0.16 * hovered,
    );
    cr.fill()?;
  }
  let icon_color = if send && model.mic_pending {
    0xf9e2af
  } else if send && !muted {
    0xa6e3a1
  } else if !send && muted {
    0xf38ba8
  } else {
    0xcdd6f4
  };
  color(cr, icon_color, 0.82 + 0.18 * hovered);
  paint_audio_device(cr, f64::from(x), f64::from(icon_y), device)?;
  if muted {
    cr.set_line_width(1.5);
    cr.move_to(f64::from(x - 7), f64::from(icon_y + 7));
    cr.line_to(f64::from(x + 7), f64::from(icon_y - 7));
    cr.stroke()?;
  }
  let Some((slider, volume)) = level else {
    return Ok(());
  };
  let volume = model.volume(slider, Some(volume)).unwrap_or(volume);
  let height = MUSIC_GAIN_BOTTOM - MUSIC_GAIN_TOP;
  color(cr, 0x45475a, 1.0);
  round(cr, x - 2, MUSIC_GAIN_TOP, 4, height, 2.0);
  cr.fill()?;
  let filled = height * i32::from(volume.percent.min(100)) / 100;
  if filled > 0 {
    color(cr, if volume.muted { 0x6c7086 } else { 0xcba6f7 }, 1.0);
    round(
      cr,
      x - 2,
      MUSIC_GAIN_BOTTOM - filled,
      4,
      filled,
      2.0_f64.min(f64::from(filled) / 2.0),
    );
    cr.fill()?;
  }
  color(cr, 0xcdd6f4, 1.0);
  let value = if volume.muted {
    "Muted".into()
  } else {
    format!("{}%", volume.percent)
  };
  label(
    cr,
    &value,
    (x, MUSIC_GAIN_BOTTOM + 5, 0),
    10,
    Weight::Normal,
    Alignment::Center,
  );
  Ok(())
}

fn paint_music_confirmation(
  cr: &Context,
  model: &Model,
  pending: &MusicConfirmation,
) -> cairo::Result<()> {
  let (left, top, width) = model.confirmation_geometry();
  let (prompt, confirm) = match pending {
    MusicConfirmation::Remove(removal) => (format!("Remove {}?", removal.title), "Remove"),
    MusicConfirmation::SignOut => ("Sign out of YouTube Music?".into(), "Sign out"),
  };
  color(cr, 0xcdd6f4, 1.0);
  label(
    cr,
    &prompt,
    (left, top, width),
    12,
    Weight::Semibold,
    Alignment::Center,
  );
  let half = width / 2 - 3;
  let y = top + 21;
  for (text, x, accent) in [
    ("Cancel", left, 0x45475a),
    (confirm, left + width - half, 0xf38ba8),
  ] {
    round(cr, x, y, half, 24, 6.0);
    color(
      cr,
      accent,
      if inside(model.bar_pointer, x, y, half, 24) {
        0.75
      } else {
        0.40
      },
    );
    cr.fill()?;
    color(cr, 0xcdd6f4, 1.0);
    label(
      cr,
      text,
      (x, y + 4, half),
      12,
      Weight::Semibold,
      Alignment::Center,
    );
  }
  Ok(())
}

fn paint_panel(cr: &Context, model: &Model) -> cairo::Result<()> {
  color(cr, 0xcdd6f4, 1.0);
  let left = PANEL_X + 12;
  if model.panel == Panel::Music {
    if !model.signed_in
      && let Some(pending) = &model.confirmation
    {
      return paint_music_confirmation(cr, model, pending);
    }
    if !model.library_checked && model.playlist_error.is_none() {
      label(
        cr,
        "Loading library…",
        (left, PANEL_Y + 21, PANEL_WIDTH - 24),
        14,
        Weight::Semibold,
        Alignment::Center,
      );
      return Ok(());
    }
    if !model.signed_in {
      let width = if model.reconnect {
        RECONNECT_WIDTH
      } else {
        SIGN_IN_WIDTH
      };
      round(cr, SIGN_IN_X, SIGN_IN_Y, width, SIGN_IN_BUTTON_HEIGHT, 9.0);
      color(cr, 0xcba6f7, 0.14 + 0.18 * model.sign_in_hover.opacity());
      cr.fill_preserve()?;
      color(cr, 0xcba6f7, 0.32 + 0.45 * model.sign_in_hover.opacity());
      cr.set_line_width(1.0);
      cr.stroke()?;
      color(cr, 0xcba6f7, 1.0);
      label(
        cr,
        if matches!(model.signing_in, Some(SignInStage::Library)) {
          "Loading your library…"
        } else if model.signing_in.is_some() {
          "Close the browser after signing in"
        } else if !model.library_checked {
          "Retry loading your library"
        } else if model.reconnect {
          "Reconnect to YouTube Music"
        } else {
          "Sign in to YouTube Music"
        },
        (SIGN_IN_X, SIGN_IN_Y + 9, width),
        if model.reconnect { 13 } else { 15 },
        Weight::Semibold,
        Alignment::Center,
      );
      if let Some(status) = model
        .playlist_error
        .as_deref()
        .filter(|_| model.signing_in.is_none())
      {
        color(cr, 0xa6adc8, 1.0);
        label(
          cr,
          status,
          (
            left,
            SIGN_IN_Y + SIGN_IN_BUTTON_HEIGHT + 5,
            PANEL_WIDTH - 24,
          ),
          13,
          Weight::Normal,
          Alignment::Center,
        );
      }
      if model.reconnect {
        sign_out_button(
          cr,
          SIGN_IN_X + RECONNECT_WIDTH + 8,
          SIGN_IN_Y,
          SIGN_IN_BUTTON_HEIGHT,
        )?;
      }
      return Ok(());
    }
    let artist = model
      .starting_song
      .as_ref()
      .map_or(model.track.artist.as_str(), |(song, _)| {
        song.artist.as_str()
      });
    if model.lyrics_view() {
      paint_music_library(cr, model, MUSIC_LEFT)?;
      let y = LYRICS_FOOTER_Y;
      picture(
        cr,
        model
          .large_cover
          .as_ref()
          .filter(|(url, _)| url == model.cover_url())
          .and_then(|(_, cover)| cover.as_ref()),
        MUSIC_LEFT,
        y + 9,
        58,
        "♪",
      )?;
      player_title(cr, model)?;
      color(cr, 0xa6adc8, 1.0);
      label(
        cr,
        model.music_status().unwrap_or(artist),
        (MUSIC_LEFT + 72, y + 40, PLAYER_X - MUSIC_LEFT - 86),
        12,
        Weight::Normal,
        Alignment::Left,
      );
      if let Some(pending) = &model.confirmation {
        return paint_music_confirmation(cr, model, pending);
      }
      paint_timeline(cr, model)?;
      cr.save()?;
      cr.translate(
        0.0,
        f64::from(PANEL_Y + TRANSPORT_Y + model.player_shift() - 5),
      );
      transport(cr, model)?;
      cr.restore()?;
      return Ok(());
    }
    picture(
      cr,
      model
        .large_cover
        .as_ref()
        .filter(|(url, _)| url == model.cover_url())
        .and_then(|(_, cover)| cover.as_ref()),
      PLAYER_X + (PLAYER_WIDTH - COVER) / 2,
      PANEL_Y + 55,
      COVER,
      "♪",
    )?;
    player_title(cr, model)?;
    color(cr, 0xa6adc8, 1.0);
    label(
      cr,
      artist,
      (PLAYER_X + 12, PANEL_Y + 246, PLAYER_WIDTH - 24),
      12,
      Weight::Normal,
      Alignment::Center,
    );
    if let Some(status) = model.music_status() {
      color(cr, 0xcba6f7, 1.0);
      label(
        cr,
        status,
        (PLAYER_X + 8, PANEL_Y + 259, PLAYER_WIDTH - 18),
        11,
        Weight::Normal,
        Alignment::Center,
      );
    }
    for channel in MusicChannel::ALL {
      paint_music_gain(cr, model, channel)?;
    }
    paint_music_library(cr, model, MUSIC_LEFT)?;
    if let Some(pending) = &model.confirmation {
      return paint_music_confirmation(cr, model, pending);
    }
    paint_timeline(cr, model)?;
    cr.save()?;
    cr.translate(0.0, f64::from(PANEL_Y + TRANSPORT_Y - 5));
    transport(cr, model)?;
    cr.restore()?;
    return Ok(());
  }

  if model.panel == Panel::Audio {
    label(
      cr,
      "Audio",
      (left, PANEL_Y + 10, PANEL_WIDTH - 24),
      14,
      Weight::Semibold,
      Alignment::Left,
    );
    let hovered = monitor_hit(model.bar_pointer);
    if hovered || model.monitoring {
      round(cr, MONITOR_X, PANEL_Y + 5, 28, 28, 6.0);
      color(cr, 0xcba6f7, if hovered { 0.25 } else { 0.16 });
      cr.fill()?;
    }
    color(cr, if model.monitoring { 0xa6e3a1 } else { 0xcdd6f4 }, 1.0);
    let x = f64::from(MONITOR_X + 14);
    let y = f64::from(PANEL_Y + 19);
    cr.set_line_width(1.4);
    cr.move_to(x - 5.0, y - 1.0);
    cr.curve_to(x - 6.0, y - 9.0, x + 7.0, y - 9.0, x + 6.0, y - 1.0);
    cr.curve_to(x + 5.0, y + 2.0, x + 2.0, y + 2.0, x + 1.0, y + 5.0);
    cr.curve_to(x, y + 9.0, x - 5.0, y + 8.0, x - 5.0, y + 4.0);
    cr.move_to(x - 2.0, y - 2.0);
    cr.curve_to(x - 2.0, y - 5.0, x + 3.0, y - 5.0, x + 3.0, y - 2.0);
    cr.stroke()?;
    for (index, (device, volume)) in [(Device::Output, model.output), (Device::Input, model.input)]
      .into_iter()
      .enumerate()
    {
      let y = audio_slider_y(index);
      let center = y + 3;
      let x = f64::from(AUDIO_BUTTON_X + AUDIO_BUTTON_SIZE / 2);
      let hovered = model.audio_hover[index].opacity();
      let muted = volume.is_some_and(|volume| volume.muted);
      round(
        cr,
        AUDIO_BUTTON_X,
        center - AUDIO_BUTTON_SIZE / 2,
        AUDIO_BUTTON_SIZE,
        AUDIO_BUTTON_SIZE,
        6.0,
      );
      color(cr, if muted { 0xf38ba8 } else { 0xcdd6f4 }, 0.16 * hovered);
      cr.fill()?;
      let center = f64::from(center);
      color(
        cr,
        if muted { 0xf38ba8 } else { 0xcdd6f4 },
        0.82 + 0.18 * hovered,
      );
      paint_audio_device(cr, x, center, device)?;
      if muted {
        cr.set_line_width(1.5);
        cr.move_to(x - 8.0, center + 8.0);
        cr.line_to(x + 8.0, center - 8.0);
        cr.stroke()?;
      }
      let slider = Slider::Device(device);
      draw_volume(cr, y, slider, model.volume(slider, volume).as_ref())?;
    }
    if let Some(error) = &model.audio_error {
      color(cr, 0xf38ba8, 1.0);
      label(
        cr,
        error,
        (
          SLIDER_X - 14,
          PANEL_Y + 113,
          PANEL_X + PANEL_WIDTH - SLIDER_X + 12,
        ),
        11,
        Weight::Normal,
        Alignment::Left,
      );
    }
    if model.count() == 0 {
      return Ok(());
    }
    color(cr, 0xa6adc8, 1.0);
    label(
      cr,
      "Applications",
      (left, PANEL_Y + 114, PANEL_WIDTH - 24),
      12,
      Weight::Semibold,
      Alignment::Left,
    );
    let offset = model.offset.min(model.count().saturating_sub(VISIBLE_ROWS));
    for (row, stream) in model
      .app_streams()
      .skip(offset)
      .take(VISIBLE_ROWS)
      .enumerate()
    {
      let y = PANEL_Y + APP_AUDIO_Y + row as i32 * ROW_HEIGHT;
      let icon = model.icons.get(&stream.icon).and_then(Option::as_ref);
      let initial = stream
        .name
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string();
      picture(cr, icon, left, y + 8, 23, &initial)?;
      color(cr, 0xcdd6f4, 1.0);
      label(
        cr,
        &stream.name,
        (AUDIO_NAME_X, y + 3, SLIDER_X - AUDIO_NAME_X - 8),
        13,
        Weight::Normal,
        Alignment::Left,
      );
      color(cr, 0xa6adc8, 1.0);
      label(
        cr,
        if stream.id.recording {
          "Microphone"
        } else {
          "Playback"
        },
        (AUDIO_NAME_X, y + 19, SLIDER_X - AUDIO_NAME_X - 8),
        10,
        Weight::Normal,
        Alignment::Left,
      );
      draw_volume(
        cr,
        y + 15,
        Slider::Stream(stream.id),
        model
          .volume(Slider::Stream(stream.id), Some(stream.volume))
          .as_ref(),
      )?;
    }
    return Ok(());
  }
  if model.panel == Panel::History {
    label(
      cr,
      "Notifications",
      (left, PANEL_Y + 10, 115),
      14,
      Weight::Semibold,
      Alignment::Left,
    );
    if !model.notices.is_empty() {
      let y = PANEL_Y + 5;
      let hovered = inside(model.bar_pointer, CLEAR_X, y, CLEAR_WIDTH, 26);
      round(cr, CLEAR_X, y, CLEAR_WIDTH, 26, 6.0);
      color(cr, 0xcdd6f4, if hovered { 0.22 } else { 0.11 });
      cr.fill()?;
      color(cr, 0xcdd6f4, 1.0);
      label(
        cr,
        "Clear all",
        (CLEAR_X, y + 5, CLEAR_WIDTH),
        12,
        Weight::Semibold,
        Alignment::Center,
      );
    }
    for (row, notice) in model
      .history()
      .skip(model.list_offset())
      .take(VISIBLE_ROWS)
      .enumerate()
    {
      let y = PANEL_Y + 35 + row as i32 * ROW_HEIGHT;
      let dismiss_x = PANEL_X + PANEL_WIDTH - 36;
      let dismiss_y = y + 3;
      let close_hovered = inside(
        model.bar_pointer,
        dismiss_x,
        dismiss_y,
        POPUP_CLOSE_SIZE,
        POPUP_CLOSE_SIZE,
      );
      if inside(
        model.bar_pointer,
        left - 4,
        y - 2,
        PANEL_WIDTH - 16,
        ROW_HEIGHT - 2,
      ) {
        round(cr, left - 4, y - 2, PANEL_WIDTH - 16, ROW_HEIGHT - 2, 6.0);
        color(cr, 0xcba6f7, 0.12);
        cr.fill()?;
      }
      if !notice.seen {
        color(cr, 0xcba6f7, 1.0);
        cr.arc(
          f64::from(left + 2),
          f64::from(y + 8),
          2.5,
          0.0,
          std::f64::consts::TAU,
        );
        cr.fill()?;
      }
      let text_x = if let Some(icon) = &notice.image {
        picture(cr, Some(icon), left + 12, y + 2, 27, "")?;
        left + 47
      } else {
        left + 12
      };
      let text_width = dismiss_x - text_x - 4;
      color(cr, if notice.seen { 0xa6adc8 } else { 0xcdd6f4 }, 1.0);
      label(
        cr,
        &notice.summary,
        (text_x, y, text_width),
        13,
        Weight::Semibold,
        Alignment::Left,
      );
      color(cr, 0xa6adc8, 1.0);
      label(
        cr,
        &notice.body,
        (text_x, y + 16, text_width),
        11,
        Weight::Normal,
        Alignment::Left,
      );
      dismissal(cr, dismiss_x, dismiss_y, close_hovered)?;
    }
  } else {
    label(
      cr,
      "Tray",
      (left, PANEL_Y + 10, PANEL_WIDTH - 24),
      14,
      Weight::Semibold,
      Alignment::Left,
    );
    for (row, item) in model
      .trays
      .iter()
      .skip(model.list_offset())
      .take(VISIBLE_ROWS)
      .enumerate()
    {
      let y = PANEL_Y + 34 + row as i32 * ROW_HEIGHT;
      if inside(
        model.bar_pointer,
        left - 4,
        y,
        PANEL_WIDTH - 16,
        ROW_HEIGHT - 2,
      ) {
        round(cr, left - 4, y, PANEL_WIDTH - 16, ROW_HEIGHT - 2, 6.0);
        color(cr, 0xcba6f7, 0.12);
        cr.fill()?;
      }
      picture(cr, item.image.as_ref(), left, y + 4, 26, "•")?;
      color(cr, 0xcdd6f4, 1.0);
      label(
        cr,
        &item.title,
        (left + 36, y + 9, PANEL_WIDTH - 60),
        13,
        Weight::Normal,
        Alignment::Left,
      );
    }
  }
  if model.count() == 0 {
    color(cr, 0xa6adc8, 1.0);
    label(
      cr,
      if model.panel == Panel::History {
        "No notifications"
      } else {
        "No tray items"
      },
      (left, PANEL_Y + 42, PANEL_WIDTH - 24),
      13,
      Weight::Normal,
      Alignment::Left,
    );
  }
  Ok(())
}

pub fn paint(cr: &Context, model: &Model, layer: Layer) -> cairo::Result<()> {
  cr.set_operator(Operator::Source);
  color(cr, 0, 0.0);
  cr.paint()?;
  cr.set_operator(Operator::Over);
  if layer == Layer::Popup {
    for (row, (notice, y)) in model.popups().zip(model.popup_rows()).enumerate() {
      cr.save()?;
      cr.translate(0.0, f64::from(y));
      paint_popup(
        cr,
        model,
        notice,
        model.popup_close_hover == Some(notice.id),
        model
          .popup_at_pointer()
          .is_some_and(|(hovered, _)| hovered == row),
      )?;
      cr.restore()?;
    }
    return Ok(());
  }
  if !model.bar_visible && model.volume_feedback.is_none() {
    return Ok(());
  }
  paint_bar(cr, model)
}

fn paint_popup(
  cr: &Context,
  model: &Model,
  notice: &Notice,
  close_hovered: bool,
  hovered: bool,
) -> cairo::Result<()> {
  round(cr, 0, 0, POPUP_WIDTH, POPUP_HEIGHT, POPUP_RADIUS);
  color(cr, if hovered { 0x29283b } else { 0x1e1e2e }, model.opacity);
  cr.fill()?;
  round(
    cr,
    1,
    1,
    POPUP_WIDTH - 2,
    POPUP_HEIGHT - 2,
    POPUP_RADIUS - 1.0,
  );
  color(cr, 0xcba6f7, 0.46);
  cr.set_line_width(1.0);
  cr.stroke()?;
  let text_x = notice.popup_text_x();
  if let Some(icon) = &notice.image {
    picture(cr, Some(icon), POPUP_PADDING, 14, 36, "")?;
  }
  let source = notice.source.as_deref();
  color(cr, 0xcdd6f4, 1.0);
  let has_header = notice.image.is_some() || source.is_some();
  label(
    cr,
    &notice.summary,
    (text_x, 12, POPUP_WIDTH - text_x - 38),
    16,
    Weight::Semibold,
    Alignment::Left,
  );
  dismissal(cr, POPUP_CLOSE_X, POPUP_CLOSE_Y, close_hovered)?;
  if let Some(source) = source {
    color(cr, 0xa6adc8, 1.0);
    label(
      cr,
      source,
      (text_x, 32, POPUP_WIDTH - text_x - 14),
      11,
      Weight::Normal,
      Alignment::Left,
    );
  }
  color(cr, 0xcdd6f4, 1.0);
  label_lines(
    cr,
    &notice.body,
    (
      POPUP_PADDING,
      if has_header { 51 } else { 36 },
      POPUP_WIDTH - 2 * POPUP_PADDING,
    ),
    13,
    Weight::Normal,
    Alignment::Left,
    if notice.popup_actions().next().is_none() && !has_header {
      3
    } else {
      2
    },
  );
  for (index, (_, action)) in notice.popup_actions().enumerate() {
    let x = POPUP_PADDING + index as i32 * (POPUP_ACTION_WIDTH + 8);
    color(cr, 0x45475a, 0.70);
    round(cr, x, POPUP_ACTION_Y, POPUP_ACTION_WIDTH, 22, 6.0);
    cr.fill()?;
    color(cr, 0xcdd6f4, 1.0);
    label(
      cr,
      action,
      (x + 6, POPUP_ACTION_Y + 3, POPUP_ACTION_WIDTH - 12),
      12,
      Weight::Semibold,
      Alignment::Center,
    );
  }
  if let Some(countdown) = &notice.countdown {
    let fraction = countdown.remaining().as_secs_f64() / countdown.duration.as_secs_f64();
    cr.save()?;
    cr.rectangle(
      0.0,
      0.0,
      f64::from(POPUP_WIDTH) * fraction,
      POPUP_RADIUS + 1.0,
    );
    cr.clip();
    round(
      cr,
      1,
      1,
      POPUP_WIDTH - 2,
      POPUP_HEIGHT - 2,
      POPUP_RADIUS - 1.0,
    );
    color(cr, 0xcba6f7, 0.92);
    cr.set_operator(Operator::Source);
    cr.set_line_width(1.0);
    cr.stroke()?;
    cr.restore()?;
  }
  Ok(())
}

pub(crate) fn bar_outline(cr: &Context, height: i32, wide_music: bool) {
  let start = f64::from(HEADER_X);
  let width = f64::from(WIDTH - HEADER_X);
  let wing = f64::from(WING);
  cr.move_to(start, 0.0);
  cr.line_to(width, 0.0);
  cr.curve_to(width - wing, 0.0, width - wing, 17.0, width - wing, 21.0);
  if height > HEIGHT {
    let (left, right) = if wide_music {
      (f64::from(MUSIC_PANEL_X), f64::from(WIDTH - MUSIC_PANEL_X))
    } else {
      (start + wing, width - wing)
    };
    let bottom = f64::from(height);
    let radius = 15.0;
    if wide_music {
      cr.curve_to(
        width - wing,
        32.0,
        width - wing + 6.0,
        40.0,
        width - wing + 19.0,
        40.0,
      );
      cr.line_to(right - radius, 40.0);
      cr.arc(
        right - radius,
        40.0 + radius,
        radius,
        -std::f64::consts::FRAC_PI_2,
        0.0,
      );
    } else {
      cr.line_to(right, f64::from(PANEL_Y));
    }
    cr.arc(
      right - radius,
      bottom - radius,
      radius,
      0.0,
      std::f64::consts::FRAC_PI_2,
    );
    cr.arc(
      left + radius,
      bottom - radius,
      radius,
      std::f64::consts::FRAC_PI_2,
      std::f64::consts::PI,
    );
    if wide_music {
      cr.arc(
        left + radius,
        40.0 + radius,
        radius,
        std::f64::consts::PI,
        std::f64::consts::PI * 1.5,
      );
      cr.line_to(start + wing - 19.0, 40.0);
      cr.curve_to(
        start + wing - 6.0,
        40.0,
        start + wing,
        32.0,
        start + wing,
        21.0,
      );
    } else {
      cr.line_to(left, 21.0);
    }
  } else {
    cr.curve_to(
      width - wing,
      32.0,
      width - wing - 7.0,
      40.0,
      width - wing - 17.0,
      40.0,
    );
    cr.line_to(start + wing + 17.0, 40.0);
    cr.curve_to(
      start + wing + 7.0,
      40.0,
      start + wing,
      32.0,
      start + wing,
      21.0,
    );
  }
  cr.curve_to(start + wing, 17.0, start + wing, 0.0, start, 0.0);
  cr.close_path();
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BarRegion {
  Header,
  Timeline,
  Lyrics,
  Search,
  MusicTitle,
  MusicList,
}

impl BarRegion {
  pub fn bounds(self, model: &Model) -> (i32, i32, i32, i32) {
    let left = MUSIC_LEFT;
    match self {
      Self::Header => (0, 0, WIDTH, HEIGHT),
      Self::Timeline => (
        PLAYER_X + 6,
        PANEL_Y + SEEK_Y - 18 + model.player_shift(),
        PLAYER_WIDTH - 14,
        24,
      ),
      Self::Lyrics => (
        MUSIC_LEFT,
        LIST_Y - 10,
        WIDTH - 2 * MUSIC_LEFT,
        LYRICS_BOTTOM - (LIST_Y - 10),
      ),
      Self::MusicTitle if model.lyrics_view() => (
        MUSIC_LEFT + 72,
        LYRICS_FOOTER_Y + 17,
        PLAYER_X - MUSIC_LEFT - 86,
        23,
      ),
      Self::MusicTitle => (PLAYER_X + 12, PANEL_Y + 222, PLAYER_WIDTH - 24, 24),
      Self::MusicList => (
        MUSIC_LEFT - 4,
        LIST_Y,
        PLAYER_X - MUSIC_LEFT + 4,
        TAB_END - LIST_Y,
      ),
      Self::Search => (left - 3, LIST_Y, PLAYER_X - left - 5, 30),
    }
  }

  pub fn paint(self, cr: &Context, model: &Model) -> cairo::Result<()> {
    if self == Self::Header {
      cr.set_operator(Operator::Clear);
      cr.paint()?;
      cr.set_operator(Operator::Over);
      paint_bar_background(cr, model)?;
      return paint_bar_header(cr, model);
    }
    // These regions are entirely inside the bar outline.
    cr.set_operator(Operator::Source);
    color(cr, 0x1e1e2e, model.opacity);
    cr.paint()?;
    cr.set_operator(Operator::Over);
    match self {
      Self::Timeline => paint_timeline(cr, model),
      Self::Lyrics => paint_lyrics(cr, model),
      Self::MusicTitle => player_title(cr, model),
      Self::MusicList => paint_music_library(cr, model, MUSIC_LEFT),
      Self::Search => {
        let left = MUSIC_LEFT;
        paint_search_field(cr, model, left, PLAYER_X - left - 8)
      }
      Self::Header => unreachable!(),
    }
  }
}

fn paint_bar_background(cr: &Context, model: &Model) -> cairo::Result<()> {
  let width = f64::from(WIDTH);
  bar_outline(cr, model.bar_height(), model.wide_music());
  let outline = cr.copy_path()?;
  cr.save()?;
  cr.rectangle(
    0.0,
    0.0,
    width,
    f64::from(model.bar_height() + SHADOW_DEPTH),
  );
  cr.set_fill_rule(cairo::FillRule::EvenOdd);
  cr.clip();
  for (stroke, alpha) in [(14.0, 0.025), (10.0, 0.035), (6.0, 0.06), (3.0, 0.11)] {
    cr.append_path(&outline);
    cr.set_line_width(stroke);
    // Fade into the top edge, where the shoulder meets the surface boundary.
    let fade = cairo::LinearGradient::new(0.0, 0.0, 0.0, 18.0);
    fade.add_color_stop_rgba(0.0, 0.0, 0.0, 0.0, 0.0);
    fade.add_color_stop_rgba(1.0, 0.0, 0.0, 0.0, alpha);
    cr.set_source(&fade)?;
    cr.stroke()?;
  }
  cr.restore()?;
  cr.append_path(&outline);
  color(cr, 0x1e1e2e, model.opacity);
  cr.fill()?;
  Ok(())
}

fn paint_bar_header(cr: &Context, model: &Model) -> cairo::Result<()> {
  for (left, hover) in BUTTONS.into_iter().zip(&model.hover) {
    let opacity = hover.opacity();
    if opacity > 0.0 {
      round(cr, left + 4, 5, UTILITY_WIDTH - 8, HEIGHT - 10, 8.0);
      color(cr, 0xcba6f7, opacity * 0.17);
      cr.fill()?;
    }
  }
  color(cr, 0xcdd6f4, 1.0);
  if let Ok(now) = DateTime::now_local().and_then(|time| time.format("%a, %b %-d   %-I:%M %p")) {
    let left = TRAY_X + UTILITY_WIDTH;
    label(
      cr,
      &now,
      (left, 11, AUDIO_X - left),
      14,
      Weight::Semibold,
      Alignment::Center,
    );
  }
  color(cr, 0xcdd6f4, 1.0);
  color(
    cr,
    if model.panel == Panel::History {
      0xcba6f7
    } else {
      0xcdd6f4
    },
    1.0,
  );
  let bell = f64::from(HISTORY_X + UTILITY_WIDTH / 2);
  cr.set_line_width(1.4);
  cr.new_sub_path();
  cr.arc(bell, 18.0, 5.0, -std::f64::consts::PI, 0.0);
  cr.line_to(bell + 5.0, 22.0);
  cr.line_to(bell + 6.0, 24.0);
  cr.line_to(bell - 6.0, 24.0);
  cr.line_to(bell - 5.0, 22.0);
  cr.close_path();
  cr.stroke()?;
  cr.arc(bell, 26.0, 1.0, 0.0, std::f64::consts::TAU);
  cr.fill()?;
  if model.do_not_disturb {
    cr.move_to(bell - 8.0, 27.0);
    cr.line_to(bell + 8.0, 12.0);
    cr.stroke()?;
  }
  if model.history().any(|notice| !notice.seen) {
    color(cr, 0xcba6f7, 1.0);
    cr.arc(bell + 9.0, 12.0, 2.5, 0.0, std::f64::consts::TAU);
    cr.fill()?;
  }
  color(
    cr,
    if model.panel == Panel::Tray {
      0xcba6f7
    } else {
      0xcdd6f4
    },
    1.0,
  );
  let tray = f64::from(TRAY_X + UTILITY_WIDTH / 2);
  for dx in [-6.0, 1.0] {
    for y in [14.0, 21.0] {
      cr.rectangle(tray + dx, y, 5.0, 5.0);
    }
  }
  cr.fill()?;
  color(
    cr,
    if model.panel == Panel::Audio {
      0xcba6f7
    } else {
      0xcdd6f4
    },
    1.0,
  );
  paint_speaker(cr, f64::from(AUDIO_X + UTILITY_WIDTH / 2), 20.0)?;
  color(
    cr,
    if model.panel == Panel::Music {
      0xcba6f7
    } else {
      0xcdd6f4
    },
    1.0,
  );
  let music = f64::from(MUSIC_X + UTILITY_WIDTH / 2);
  cr.new_sub_path();
  cr.arc(music - 2.0, 24.0, 3.0, 0.0, std::f64::consts::TAU);
  cr.fill()?;
  cr.move_to(music + 0.5, 24.0);
  cr.line_to(music + 0.5, 13.0);
  cr.curve_to(music + 1.5, 15.0, music + 4.0, 15.0, music + 4.5, 18.0);
  cr.stroke()?;
  Ok(())
}

fn paint_bar(cr: &Context, model: &Model) -> cairo::Result<()> {
  paint_bar_background(cr, model)?;
  paint_bar_header(cr, model)?;
  if model.panel != Panel::None {
    paint_panel(cr, model)?;
  }
  if let Some((_, volume)) = model.volume_feedback {
    let left = PANEL_X + 15;
    let x = left + 25;
    let y = model.panel_bottom();
    let width = PANEL_WIDTH - 30 - 25 - 52;
    let text = if volume.muted {
      "Muted".into()
    } else {
      format!("{}%", volume.percent)
    };
    color(cr, 0xcdd6f4, 1.0);
    paint_speaker(cr, f64::from(left + 7), f64::from(y + 14))?;
    label(
      cr,
      &text,
      (x + width + 6, y + 5, 46),
      13,
      Weight::Semibold,
      Alignment::Right,
    );
    color(cr, 0x45475a, 1.0);
    round(cr, x, y + 12, width, 5, 2.5);
    cr.fill()?;
    let filled = width * i32::from(volume.percent.min(100)) / 100;
    if filled > 0 {
      color(cr, if volume.muted { 0x7f849c } else { 0xcba6f7 }, 1.0);
      round(
        cr,
        x,
        y + 12,
        filled,
        5,
        2.5_f64.min(f64::from(filled) / 2.0),
      );
      cr.fill()?;
    }
  }
  Ok(())
}
