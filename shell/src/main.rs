mod audio;
mod media;
mod services;
mod ui;
mod wayland;

use std::cell::{Cell, RefCell};
use std::ffi::{CString, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use audio::{Device, Route};
use gio::glib::{self, ControlFlow, IOCondition, MainContext, MainLoop};
use services::Services;
use ui::{Action, AudioChange, Model, MusicTab, Panel, Slider};
use wayland::Desktop;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const HELP: &str = "Wayland desktop shell

Usage: lintel --wallpaper IMAGE [OPTIONS]
       lintel player [YTM ARGUMENTS]
       lintel music [LIBRARY ARGUMENTS]
       lintel toggle-audio
       lintel volume up|down
       lintel toggle-picker NAMESPACE COMMAND [ARGS]

Options:
  --wallpaper IMAGE       Wallpaper image
  --backdrop-blur SIGMA   Create a blurred backdrop (0–1024 pixels)
  --playlist NAME=ID     Pin a YouTube Music playlist (repeatable)
  -h, --help             Show help";

fn main() -> ExitCode {
  if let Err(error) = run() {
    eprintln!("lintel: {error}");
    ExitCode::FAILURE
  } else {
    ExitCode::SUCCESS
  }
}

fn run() -> Result<()> {
  let mut args = std::env::args().skip(1).peekable();
  if args.peek().is_some_and(|arg| arg == "music") {
    return Err(
      Command::new("lintel-ytmusic")
        .args(args.skip(1))
        .exec()
        .into(),
    );
  }
  if args.peek().is_some_and(|arg| arg == "player") {
    return Err(Command::new("ytm").args(args.skip(1)).exec().into());
  }
  if args.peek().is_some_and(|arg| arg == "toggle-audio") {
    if args.nth(1).is_some() {
      return Err("toggle-audio takes no arguments".into());
    }
    return services::toggle_audio();
  }
  if args.peek().is_some_and(|arg| arg == "volume") {
    let step = match args.nth(1).as_deref() {
      Some("up") => 5,
      Some("down") => -5,
      _ => return Err("expected volume up or down".into()),
    };
    if args.next().is_some() {
      return Err("volume takes one argument".into());
    }
    return services::step_volume(step);
  }
  if args.peek().is_some_and(|arg| arg == "toggle-picker") {
    return toggle_picker(args.skip(1));
  }
  let mut wallpaper = None;
  let mut blur = None;
  let mut pinned = Vec::new();
  while let Some(arg) = args.next() {
    match arg.as_str() {
      "--help" | "-h" => {
        println!("{HELP}");
        return Ok(());
      }
      "--wallpaper" => wallpaper = Some(args.next().ok_or("missing wallpaper image")?),
      "--backdrop-blur" => blur = Some(args.next().ok_or("missing blur amount")?.parse::<u32>()?),
      "--playlist" => pinned.push(media::Playlist::pin(
        &args.next().ok_or("missing playlist")?,
      )?),
      _ => return Err(format!("unknown argument {arg}. See lintel --help.").into()),
    }
  }
  let image = wallpaper.ok_or("--wallpaper IMAGE is required")?;
  if !Path::new(&image).is_file() {
    return Err(format!("wallpaper image is missing or unreadable: {image}").into());
  }
  let wallpaper =
    rustbg::Config::new(&image, "rustbg-wallpaper", 0).ok_or("invalid wallpaper path")?;
  let backdrop = blur
    .map(|strength| {
      rustbg::Config::new(&image, "rustbg-backdrop", strength).ok_or("invalid backdrop blur amount")
    })
    .transpose()?;

  let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR is required")?;
  let main_loop = MainLoop::new(None, false);
  let model = Rc::new(RefCell::new(Model::default()));
  model.borrow_mut().playlists = pinned.clone();
  model.borrow_mut().music_mix = audio::music_mix_enabled();
  model.borrow_mut().opacity = std::env::var("LINTEL_OPACITY")
    .ok()
    .and_then(|value| value.parse::<f64>().ok())
    .filter(|value| (0.0..=1.0).contains(value))
    .unwrap_or(0.8);
  let display = Rc::new(Display {
    desktop: RefCell::new(Desktop::connect(model.clone())?),
    main_loop: main_loop.clone(),
    failed: RefCell::new(None),
    wants_write: Cell::new(false),
    animating: Cell::new(false),
    search_timer: RefCell::new(None),
    model: model.clone(),
  });

  let target = display.clone();
  let refresh: Rc<dyn Fn()> = Rc::new(move || target.draw(true));
  let services = Services::connect(model.clone(), refresh.clone())?;
  initialize_wallpaper_environment();
  for config in std::iter::once(wallpaper).chain(backdrop) {
    thread::Builder::new()
      .name("wallpaper".into())
      .stack_size(512 * 1024)
      .spawn(move || rustbg::run(config))?;
  }

  let socket = Path::new(&runtime).join("yt-music-player/ytmd.sock");
  let (router, route_updates, route_worker) = audio::route_worker();
  let route_model = model.clone();
  let route_refresh = refresh.clone();
  MainContext::default().spawn_local(async move {
    while let Ok(update) = route_updates.recv().await {
      let mut model = route_model.borrow_mut();
      if model.mic_revision != update.revision {
        continue;
      }
      model.mic_route = update.enabled;
      model.mic_pending = false;
      model.mic_error = update.error;
      drop(model);
      route_refresh();
    }
  });

  let watch_player = {
    let socket = socket.clone();
    let model = model.clone();
    let refresh = refresh.clone();
    let router = router.clone();
    let watching = Cell::new(false);
    move || {
      if !watching.replace(true) {
        MainContext::default().spawn_local(watch_tracks(
          media::subscribe_tracks(socket.clone()),
          model.clone(),
          refresh.clone(),
          router.clone(),
          socket.clone(),
        ));
      }
    }
  };
  if socket.exists() {
    watch_player();
  }

  let audio_model = model.clone();
  let audio_refresh = refresh.clone();
  let step_model = model.clone();
  let step_refresh = refresh.clone();
  services.bind_audio_panel(
    move || {
      let mut state = audio_model.borrow_mut();
      let open = state.panel != Panel::Audio;
      state.select(if open { Panel::Audio } else { Panel::None });
      state.bar_visible = open;
      state.pointer_generation = state.pointer_generation.wrapping_add(1);
      drop(state);
      audio_refresh();
      if open {
        watch_audio(&audio_model, &audio_refresh);
      }
    },
    move |step| queue_volume(&step_model, &step_refresh, AudioChange::OutputStep(step)),
  )?;

  let fd = display.desktop.borrow().fd();
  let display_events = display.clone();
  let redraw = refresh.clone();
  let actions_router = router.clone();
  let monitor = Rc::new(RefCell::new(None::<audio::AudioHelper>));
  glib::unix_fd_add_local(
    fd,
    IOCondition::IN | IOCondition::HUP | IOCondition::ERR,
    move |_, _| {
      let dispatched = display_events.desktop.borrow_mut().dispatch();
      let actions = match dispatched {
        Ok(actions) => actions,
        Err(error) => {
          display_events.fail(error);
          return ControlFlow::Break;
        }
      };
      for action in actions {
        match action {
          Action::Media { command, rewind } => media::control(command, rewind),
          Action::Seek(seconds) => {
            let mut state = model.borrow_mut();
            let target = (state.track.id.clone(), seconds);
            let position = state.track.position;
            let progressing = state.track.progressing_since;
            state.pending_seek = Some(target.clone());
            state.playlist_error = None;
            drop(state);
            let model = model.clone();
            let redraw = redraw.clone();
            redraw();
            background(
              move || media::seek(seconds),
              move |succeeded| {
                let mut state = model.borrow_mut();
                if state.pending_seek.as_ref() != Some(&target) {
                  return;
                }
                state.pending_seek = None;
                if !succeeded {
                  state.playlist_error = Some("Could not seek in this track".into());
                } else if state.track.position == position
                  && state.track.progressing_since == progressing
                {
                  state.track.position = seconds;
                  state.track.progressing_since = progressing.map(|_| Instant::now());
                }
                drop(state);
                redraw();
              },
            );
          }
          Action::DismissBar => {
            let mut state = model.borrow_mut();
            if state.dismiss_confirmation() {
              drop(state);
              fetch_small_covers(&model, &redraw);
              redraw();
            } else if state.panel != Panel::None {
              state.select(Panel::None);
              state.bar_visible = false;
              drop(state);
              redraw();
            }
          }
          Action::BarFocusLost => {
            let generation = {
              let state = model.borrow();
              if state.bar_hovered() {
                continue;
              }
              state.pointer_generation
            };
            let model = model.clone();
            let redraw = redraw.clone();
            glib::timeout_add_local_once(Duration::from_millis(90), move || {
              let mut state = model.borrow_mut();
              if state.pointer_generation == generation
                && !state.bar_hovered()
                && state.panel != Panel::None
              {
                state.select(Panel::None);
                state.bar_visible = false;
                drop(state);
                redraw();
              }
            });
          }
          Action::MusicTab(tab) => {
            if tab == MusicTab::Lyrics {
              fetch_lyrics(&model, &redraw);
            } else {
              fetch_small_covers(&model, &redraw);
            }
          }
          Action::Search(query) => search_music(query, &model, &redraw),
          Action::AddSong(playlist, song) => {
            let mut state = model.borrow_mut();
            if state.adding_song || !state.signed_in {
              continue;
            }
            state.adding_song = true;
            state.add_feedback = Some(format!("Adding to {}…", playlist.name));
            drop(state);
            let model = model.clone();
            let redraw = redraw.clone();
            let socket = socket.clone();
            let id = playlist.id.clone();
            background(
              move || media::add_song(&id, &song.id).map_err(|error| error.to_string()),
              move |result| {
                let mut state = model.borrow_mut();
                state.adding_song = false;
                if !state.signed_in {
                  return;
                }
                let sync = result.is_ok()
                  && state
                    .selected_playlist
                    .as_ref()
                    .is_some_and(|active| active.id == playlist.id);
                state.add_feedback = Some(match result {
                  Ok(()) => format!("Added to {}", playlist.name),
                  Err(error) => error,
                });
                drop(state);
                if sync {
                  refresh_active(&model, &redraw, &socket);
                }
                fetch_small_covers(&model, &redraw);
                redraw();
              },
            );
          }
          Action::PlayResult(song) => {
            watch_player();
            let mut state = model.borrow_mut();
            if state.playlist_busy {
              continue;
            }
            state.remember_playlist();
            state.playlist_busy = true;
            state.playlist_error = None;
            drop(state);
            let socket = socket.clone();
            let model = model.clone();
            let redraw = redraw.clone();
            background(
              move || media::play_result(&socket, song).map_err(|error| error.to_string()),
              move |result| {
                let mut state = model.borrow_mut();
                state.playlist_busy = false;
                match result {
                  Ok(()) => {
                    state.selected_playlist = None;
                    media::save_playlist(None);
                    state.playlist_epoch = state.playlist_epoch.wrapping_add(1);
                    state.playlist_syncing = false;
                  }
                  Err(error) => state.playlist_error = Some(error),
                }
                drop(state);
                redraw();
              },
            );
          }
          Action::Reorder { expected, from, to } => {
            let socket = socket.clone();
            let model = model.clone();
            let redraw = redraw.clone();
            background(
              move || {
                media::move_queue(&socket, expected.as_ref().clone(), from, to)
                  .map_err(|error| error.to_string())
              },
              move |result| {
                if let Err(error) = result {
                  let mut state = model.borrow_mut();
                  state.cancel_queue_drag();
                  state.playlist_error = Some(error);
                  drop(state);
                  redraw();
                }
              },
            );
          }
          Action::OpenAudio => watch_audio(&model, &redraw),
          Action::ToggleMonitor => {
            if monitor.borrow_mut().take().is_some() {
              model.borrow_mut().monitoring = false;
            } else {
              match audio::AudioHelper::monitoring() {
                Ok(helper) => {
                  let pid = helper.pid();
                  monitor.replace(Some(helper));
                  let mut state = model.borrow_mut();
                  state.monitoring = true;
                  state.audio_error = None;
                  drop(state);
                  let monitor = monitor.clone();
                  let model = model.clone();
                  let redraw = redraw.clone();
                  glib::timeout_add_local(Duration::from_millis(500), move || {
                    let mut helper = monitor.borrow_mut();
                    let Some(process) = helper.as_mut().filter(|process| process.pid() == pid)
                    else {
                      return ControlFlow::Break;
                    };
                    if !process.exited() {
                      return ControlFlow::Continue;
                    }
                    helper.take();
                    let mut state = model.borrow_mut();
                    state.monitoring = false;
                    state.audio_error = Some("Mic monitoring stopped".into());
                    drop(state);
                    redraw();
                    ControlFlow::Break
                  });
                }
                Err(error) => model.borrow_mut().audio_error = Some(error.to_string()),
              }
            }
            redraw();
          }
          Action::SmallCovers => fetch_small_covers(&model, &redraw),
          Action::Volume(target, value) => {
            queue_volume(&model, &redraw, AudioChange::Slider(target, value));
          }
          Action::ToggleMic => {
            watch_player();
            let mut state = model.borrow_mut();
            state.mic_route = !state.mic_route;
            state.mic_pending = state.mic_route;
            state.mic_revision = state.mic_revision.wrapping_add(1);
            state.mic_error = None;
            let _ = actions_router.send(Route::Update {
              enabled: state.mic_route,
              revision: state.mic_revision,
            });
            drop(state);
            redraw();
          }
          Action::OpenMusic | Action::SignIn => {
            let sign_in = matches!(action, Action::SignIn);
            if !sign_in && (socket.exists() || media::has_saved_player()) {
              watch_player();
            }
            let use_cache = !sign_in && !model.borrow().library_checked;
            if use_cache && let Some(playlists) = media::cached_library() {
              model.borrow_mut().set_library(&pinned, playlists);
              redraw();
            }
            watch_music_clock(&model, &display_events);
            watch_audio(&model, &redraw);
            fetch_art(&model, &redraw);
            fetch_small_covers(&model, &redraw);
            let lyrics = {
              let state = model.borrow();
              state.music_tab == MusicTab::Lyrics && state.signed_in
            };
            if lyrics {
              fetch_lyrics(&model, &redraw);
            }

            let mut state = model.borrow_mut();
            if state.library_refreshing
              || state.playlist_busy
              || (!sign_in
                && state
                  .library_loaded
                  .is_some_and(|loaded| loaded.elapsed() < Duration::from_secs(60)))
            {
              continue;
            }
            state.library_refreshing = true;
            state.signing_in = sign_in.then_some(ui::SignInStage::Browser);
            if sign_in || !state.signed_in {
              state.playlist_error = None;
            }
            let revision = state.library_revision;
            drop(state);
            let progress = sign_in.then(|| {
              let (send, receive) = async_channel::bounded(1);
              let model = model.clone();
              let redraw = redraw.clone();
              MainContext::default().spawn_local(async move {
                if receive.recv().await.is_ok() {
                  let mut state = model.borrow_mut();
                  if state.library_revision == revision && state.signing_in.is_some() {
                    state.signing_in = Some(ui::SignInStage::Library);
                    drop(state);
                    redraw();
                  }
                }
              });
              send
            });
            let model = model.clone();
            let redraw = redraw.clone();
            let pinned = pinned.clone();
            background(
              move || {
                if let Some(progress) = progress {
                  media::sign_in(|| {
                    let _ = progress.try_send(());
                  })
                  .map_err(|error| error.to_string())
                } else {
                  media::playlists().map_err(|error| error.to_string())
                }
              },
              move |result| {
                let mut state = model.borrow_mut();
                if state.library_revision != revision {
                  return;
                }
                state.library_refreshing = false;
                state.signing_in = None;
                state.library_checked |= result.is_ok();
                match result {
                  Ok(media::Library::Connected(lists)) => {
                    state.set_library(&pinned, lists);
                    state.library_loaded = Some(std::time::Instant::now());
                  }
                  Ok(media::Library::Disconnected) => {
                    state.signed_in = false;
                    state.reconnect = false;
                    state.library_loaded = None;
                  }
                  Ok(media::Library::Expired) => {
                    state.signed_in = false;
                    state.reconnect = true;
                    state.library_loaded = None;
                  }
                  Err(error) if sign_in || !state.signed_in => state.playlist_error = Some(error),
                  Err(_) => {}
                }
                drop(state);
                fetch_art(&model, &redraw);
                fetch_small_covers(&model, &redraw);
                redraw();
              },
            );
          }
          Action::SignOut => {
            let mut state = model.borrow_mut();
            state.playlist_busy = true;
            state.library_revision = state.library_revision.wrapping_add(1);
            state.library_refreshing = false;
            drop(state);
            let model = model.clone();
            let redraw = redraw.clone();
            let pinned = pinned.clone();
            let player_running = socket.exists();
            background(
              move || media::session_command("logout").map_err(|error| error.to_string()),
              move |result| {
                let mut state = model.borrow_mut();
                state.playlist_busy = false;
                state.playlist_error = result.err();
                if state.playlist_error.is_none() {
                  state.signed_in = false;
                  state.reconnect = false;
                  state.library_loaded = None;
                  state.selected_playlist = None;
                  media::save_playlist(None);
                  state.playlist_bookmarks.clear();
                  state.playlists = pinned;
                  state.small_covers.clear();
                  state.large_cover = None;
                  state.starting_song = None;
                  state.search_query.clear();
                  state.search_cursor = 0;
                  state.search_edited();
                  state.lyrics_id.clear();
                  state.lyrics = Default::default();
                  state.lyrics_status = None;
                  state.playlist_epoch = state.playlist_epoch.wrapping_add(1);
                  state.playlist_syncing = false;
                  if player_running {
                    media::control("pause", false);
                  }
                }
                drop(state);
                redraw();
              },
            );
          }
          Action::Play(playlist) => {
            watch_player();
            let mut state = model.borrow_mut();
            if state.playlist_busy
              || state
                .selected_playlist
                .as_ref()
                .is_some_and(|active| active.id == playlist.id)
            {
              continue;
            }
            state.remember_playlist();
            let bookmark = state.playlist_bookmarks.get(&playlist.id).cloned();
            state.playlist_epoch = state.playlist_epoch.wrapping_add(1);
            state.playlist_syncing = false;
            let epoch = state.playlist_epoch;
            state.playlist_busy = true;
            state.loading_playlist = Some(playlist.id.clone());
            state.playlist_error = None;
            drop(state);
            redraw();

            let socket = socket.clone();
            let model = model.clone();
            let redraw = redraw.clone();
            background(
              move || {
                media::prepare_playlist(playlist, bookmark).map_err(|error| error.to_string())
              },
              move |prepared| {
                let finish = {
                  let model = model.clone();
                  let redraw = redraw.clone();
                  let socket = socket.clone();
                  move |result: std::result::Result<media::Playlist, String>| {
                    let mut state = model.borrow_mut();
                    state.playlist_busy = false;
                    state.loading_playlist = None;
                    match result {
                      Ok(playlist) => {
                        if let Some(entry) = state
                          .playlists
                          .iter_mut()
                          .find(|entry| entry.id == playlist.id)
                          && entry.art.is_empty()
                        {
                          entry.art.clone_from(&playlist.art);
                        }
                        if state
                          .starting_song
                          .as_ref()
                          .is_some_and(|(song, _)| song.id == state.track.id)
                        {
                          state.starting_song = None;
                        }
                        state.selected_playlist = Some(playlist);
                        media::save_playlist(state.selected_playlist.as_ref());
                      }
                      Err(error) => {
                        state.starting_song = None;
                        state.playlist_error = Some(error);
                      }
                    }
                    let delay = state
                      .selected_playlist
                      .as_ref()
                      .map(|active| if active.partial { 1 } else { 15 });
                    drop(state);
                    fetch_art(&model, &redraw);
                    fetch_small_covers(&model, &redraw);
                    redraw();
                    if let Some(delay) = delay {
                      watch_playlist(model, redraw, socket, epoch, delay);
                    }
                  }
                };
                let prepared = match prepared {
                  Ok(prepared) => prepared,
                  Err(error) => {
                    finish(Err(error));
                    return;
                  }
                };
                model.borrow_mut().starting_song = prepared
                  .first_song()
                  .map(|(song, position)| (song.clone(), position));
                redraw();
                fetch_art(&model, &redraw);
                background(
                  move || prepared.play(&socket).map_err(|error| error.to_string()),
                  finish,
                );
              },
            );
          }
          Action::RemoveSong(removal) => {
            let mut state = model.borrow_mut();
            let Some(previous) = state
              .selected_playlist
              .clone()
              .filter(|list| list.id == removal.playlist)
            else {
              continue;
            };
            if state.playlist_busy || !state.signed_in {
              continue;
            }
            let song = removal.song;
            let queue = state.track.queue.clone();
            state.playlist_epoch = state.playlist_epoch.wrapping_add(1);
            let epoch = state.playlist_epoch;
            state.playlist_syncing = false;
            state.playlist_busy = true;
            state.playlist_error = None;
            drop(state);
            redraw();
            let socket = socket.clone();
            let player = socket.clone();
            let model = model.clone();
            let redraw = redraw.clone();
            background(
              move || {
                media::remove_song(&player, previous, queue, &song)
                  .map_err(|error| error.to_string())
              },
              move |result| {
                let mut state = model.borrow_mut();
                if state.playlist_epoch != epoch {
                  return;
                }
                state.playlist_busy = false;
                match result {
                  Ok(playlist) => {
                    state.selected_playlist = Some(playlist);
                    media::save_playlist(state.selected_playlist.as_ref());
                  }
                  Err(error) => state.playlist_error = Some(error),
                }
                drop(state);
                redraw();
                watch_playlist(model, redraw, socket, epoch, 15);
              },
            );
          }
          Action::HideBar(generation) => {
            let model = model.clone();
            let redraw = redraw.clone();
            glib::timeout_add_local_once(Duration::from_millis(90), move || {
              let mut state = model.borrow_mut();
              if state.pointer_generation == generation && state.panel == Panel::None {
                state.bar_visible = false;
                drop(state);
                redraw();
              }
            });
          }
          Action::Close(id) => services.close(id, 2),
          Action::PopupHover => services.hover_popup(),
          Action::ToggleDoNotDisturb => services.toggle_do_not_disturb(),
          Action::ClearNotifications => services.clear_notifications(),
          Action::Invoke(id, action) => services.invoke(id, &action),
          Action::Tray(address, method) => services.tray_action(&address, method),
        }
      }
      display_events.draw(false);
      ControlFlow::Continue
    },
  );

  fn schedule_clock(refresh: Rc<dyn Fn()>) {
    let delta = SystemTime::now()
      .duration_since(SystemTime::UNIX_EPOCH)
      .unwrap_or_default();
    let wait = Duration::from_millis((60_000 - delta.as_millis() % 60_000) as u64);
    glib::timeout_add_local_once(wait, move || {
      refresh();
      schedule_clock(refresh);
    });
  }
  schedule_clock(refresh);
  for signal in [libc::SIGINT, libc::SIGTERM] {
    let shutdown = main_loop.clone();
    glib::unix_signal_add_local(signal, move || {
      shutdown.quit();
      ControlFlow::Break
    });
  }
  main_loop.run();
  let _ = router.send(Route::Stop);
  let _ = route_worker.join();
  display.failed.borrow_mut().take().map_or(Ok(()), Err)
}

fn toggle_picker(mut args: impl Iterator<Item = String>) -> Result<()> {
  use rustix::event::{PollFd, PollFlags, Timespec, poll};
  use rustix::io::Errno;
  use rustix::process::{Pid, PidfdFlags, Signal, pidfd_open, pidfd_send_signal};

  let namespace = args.next().ok_or("missing picker namespace")?;
  let program = args.next().ok_or("missing picker command")?;
  let output = Command::new("niri")
    .args(["msg", "--json", "layers"])
    .output()?;
  if !output.status.success() {
    return Err("could not query Niri layers".into());
  }
  #[derive(serde::Deserialize)]
  struct PickerLayer {
    namespace: String,
    pid: Option<i32>,
  }
  let layers: Vec<PickerLayer> = serde_json::from_slice(&output.stdout)?;
  for layer in layers.into_iter().filter(|layer| {
    matches!(
      layer.namespace.as_str(),
      "session-apps" | "session-clipboard"
    ) || layer.namespace == namespace
  }) {
    let Some(pid) = layer.pid.and_then(Pid::from_raw) else {
      continue;
    };
    let process = match pidfd_open(pid, PidfdFlags::empty()) {
      Ok(process) => process,
      Err(Errno::SRCH) => continue,
      Err(error) => return Err(error.into()),
    };
    if !std::fs::read_link(format!("/proc/{}/exe", pid.as_raw_nonzero()))
      .is_ok_and(|exe| exe.file_name().is_some_and(|name| name == "fuzzel"))
    {
      continue;
    }
    match pidfd_send_signal(&process, Signal::TERM) {
      Ok(()) if layer.namespace == namespace => return Ok(()),
      Ok(()) => {
        // Fuzzel keeps its per-display lock until the process exits.
        let mut wait = [PollFd::new(&process, PollFlags::IN)];
        if poll(
          &mut wait,
          Some(&Timespec {
            tv_sec: 1,
            tv_nsec: 0,
          }),
        )? == 0
        {
          return Err("previous picker did not close".into());
        }
      }
      Err(Errno::SRCH) => {}
      Err(error) => return Err(error.into()),
    }
  }
  Err(Command::new(program).args(args).exec().into())
}

async fn watch_tracks(
  updates: async_channel::Receiver<media::Track>,
  model: Rc<RefCell<Model>>,
  refresh: Rc<dyn Fn()>,
  router: std::sync::mpsc::Sender<Route>,
  socket: PathBuf,
) {
  let mut restore_attempted = false;
  while let Ok(next) = updates.recv().await {
    let mut state = model.borrow_mut();
    if state.track == next {
      continue;
    }
    let previous_art = state.cover_url().to_owned();
    let queue_changed = state.track.queue_index != next.queue_index
      || !std::sync::Arc::ptr_eq(&state.track.queue_songs, &next.queue_songs);
    let track_changed = state.track.id != next.id;
    let lyric_metadata_loaded = !state.track.has_lyric_metadata() && next.has_lyric_metadata();
    if !restore_attempted && next.has_track && !next.queue.is_empty() {
      restore_attempted = true;
      if state.playlist_epoch == 0 {
        let queue = next.queue.clone();
        let expected = queue.clone();
        let current = next.id.clone();
        let model = model.clone();
        let refresh = refresh.clone();
        let socket = socket.clone();
        background(
          move || media::restore_playlist(&queue, &current),
          move |playlist| {
            let Some(playlist) = playlist else { return };
            let mut state = model.borrow_mut();
            if state.playlist_epoch == 0 && state.track.has_track && state.track.queue == expected {
              let delay = if playlist.partial { 1 } else { 15 };
              media::save_playlist(Some(&playlist));
              state.selected_playlist = Some(playlist);
              drop(state);
              refresh();
              watch_playlist(model, refresh, socket, 0, delay);
            }
          },
        );
      }
    }
    if state.mic_route
      && (state.track.player_pid != next.player_pid
        || state.track.playing != next.playing
        || state.track.id != next.id)
    {
      let _ = router.send(Route::Update {
        enabled: true,
        revision: state.mic_revision,
      });
    }
    state.update_track(next);
    let expanded = state.panel == Panel::Music;
    let art_changed = previous_art != state.cover_url();
    let lyrics =
      expanded && state.music_tab == MusicTab::Lyrics && (track_changed || lyric_metadata_loaded);
    drop(state);
    if lyrics {
      fetch_lyrics(&model, &refresh);
    }
    if expanded && art_changed {
      fetch_art(&model, &refresh);
    }
    if expanded && queue_changed {
      fetch_small_covers(&model, &refresh);
    }
    refresh();
  }
}

fn watch_music_clock(model: &Rc<RefCell<Model>>, display: &Rc<Display>) {
  if std::mem::replace(&mut model.borrow_mut().music_clock, true) {
    return;
  }
  music_clock_tick(model.clone(), display.clone(), None, None);
}

fn music_clock_tick(
  model: Rc<RefCell<Model>>,
  display: Rc<Display>,
  last_position: Option<u64>,
  last_lyric: Option<(usize, u32, bool)>,
) {
  let mut state = model.borrow_mut();
  if state.panel != Panel::Music {
    state.music_clock = false;
    return;
  }
  let position = (state.track.elapsed() * 4.0) as u64;
  let playing = state.timeline_animating();
  let timeline_changed = playing && last_position != Some(position);
  let lyric = state.lyric_marker();
  let lyrics_changed = lyric.is_some() && last_lyric != lyric;
  drop(state);
  if timeline_changed || lyrics_changed {
    let mut desktop = display.desktop.borrow_mut();
    if timeline_changed {
      desktop.repaint_bar(ui::BarRegion::Timeline);
    }
    if lyrics_changed {
      desktop.repaint_bar(ui::BarRegion::Lyrics);
    }
    drop(desktop);
    display.draw(false);
  }
  let delay = if playing && lyric.is_some() { 50 } else { 250 };
  glib::timeout_add_local_once(Duration::from_millis(delay), move || {
    music_clock_tick(model, display, Some(position), lyric);
  });
}

fn watch_audio(model: &Rc<RefCell<Model>>, refresh: &Rc<dyn Fn()>) {
  refresh_audio(model, refresh);
  if std::mem::replace(&mut model.borrow_mut().audio_watching, true) {
    return;
  }
  let model = model.clone();
  let refresh = refresh.clone();
  glib::timeout_add_local(Duration::from_secs(1), move || {
    if !matches!(model.borrow().panel, Panel::Music | Panel::Audio) {
      model.borrow_mut().audio_watching = false;
      return ControlFlow::Break;
    }
    refresh_audio(&model, &refresh);
    ControlFlow::Continue
  });
}

fn queue_volume(model: &Rc<RefCell<Model>>, refresh: &Rc<dyn Fn()>, change: AudioChange) {
  let mut state = model.borrow_mut();
  state.audio_revision = state.audio_revision.wrapping_add(1);
  if let AudioChange::Slider(target, Some(value)) = change {
    state.apply_volume(target, Some(value), None);
  }
  match (state.audio_requests.back_mut(), change) {
    (Some(AudioChange::Slider(previous, Some(last))), AudioChange::Slider(target, Some(value)))
      if *previous == target =>
    {
      *last = value
    }
    (Some(AudioChange::OutputStep(last)), AudioChange::OutputStep(step))
      if last.signum() == step.signum() =>
    {
      *last = (*last + step).clamp(-100, 100)
    }
    _ => state.audio_requests.push_back(change),
  }
  drop(state);
  write_volume(model, refresh);
}

fn show_volume(model: &Rc<RefCell<Model>>, refresh: &Rc<dyn Fn()>, volume: audio::Volume) {
  let active = model
    .borrow_mut()
    .volume_feedback
    .replace((std::time::Instant::now(), volume));
  if active.is_some() {
    return;
  }
  let model = model.clone();
  let refresh = refresh.clone();
  glib::timeout_add_local(Duration::from_millis(100), move || {
    let mut state = model.borrow_mut();
    if state
      .volume_feedback
      .is_some_and(|(started, _)| started.elapsed() < Duration::from_millis(1100))
    {
      return ControlFlow::Continue;
    }
    state.volume_feedback = None;
    drop(state);
    refresh();
    ControlFlow::Break
  });
}

fn write_volume(model: &Rc<RefCell<Model>>, refresh: &Rc<dyn Fn()>) {
  let mut state = model.borrow_mut();
  if state.audio_setting {
    return;
  }
  let Some(change) = state.audio_requests.pop_front() else {
    return;
  };
  let target = change.target();
  let value = match change {
    AudioChange::Slider(_, value) => value,
    AudioChange::OutputStep(_) => None,
  };
  let group = match target {
    Slider::Stream(id) | Slider::Music(id) => state
      .streams
      .iter()
      .find(|stream| stream.id == id)
      .map(|stream| (stream.members.clone(), !stream.volume.muted)),
    Slider::Device(_) | Slider::MusicLevel(..) => None,
  };
  state.audio_setting = true;
  drop(state);
  let model = model.clone();
  let refresh = refresh.clone();
  background(
    move || match (target, change) {
      (_, AudioChange::OutputStep(step)) => audio::step_output(step).map(Some),
      (Slider::Device(device), _) => audio::set(device, value).map(Some),
      (Slider::MusicLevel(channel, node), _) => audio::set_music(channel, node, value).map(Some),
      (Slider::Stream(_) | Slider::Music(_), _) => {
        let Some((members, muted)) = group else {
          return Ok(None);
        };
        let mut result = Ok(());
        for member in members {
          let updated = audio::set_stream(member, value, muted);
          result = result.and(updated);
        }
        result.map(|()| None)
      }
    },
    move |result| {
      if let (AudioChange::OutputStep(_), Ok(Some(volume))) = (change, &result) {
        show_volume(&model, &refresh, *volume);
      }
      let mut state = model.borrow_mut();
      state.audio_setting = false;
      match result {
        Ok(confirmed) => {
          state.audio_error = None;
          if !state
            .audio_requests
            .iter()
            .any(|request| request.target() == target)
          {
            if confirmed.is_some() || value.is_none() {
              state.apply_volume(target, value, confirmed);
            }
          }
        }
        Err(error) => state.audio_error = Some(error.to_string()),
      }
      let pending = !state.audio_requests.is_empty();
      let failed = state.audio_error.is_some();
      drop(state);
      if pending {
        write_volume(&model, &refresh);
      } else if failed {
        refresh_audio(&model, &refresh);
      }
      refresh();
    },
  );
}

fn refresh_audio(model: &Rc<RefCell<Model>>, refresh: &Rc<dyn Fn()>) {
  let state = model.borrow();
  let music_only = state.panel == Panel::Music;
  let music_mix = state.music_mix;
  let split_music = music_only && music_mix;
  let player = state.track.player_pid;
  if state.audio_setting || (music_only && (!state.signed_in || (!split_music && player == 0))) {
    return;
  }
  let revision = state.audio_revision;
  drop(state);
  let model = model.clone();
  let refresh = refresh.clone();
  background(
    move || {
      if split_music {
        return (None, None, Some(audio::music_levels()));
      }
      let devices = (!music_only).then(|| {
        (
          audio::get(Device::Output).ok(),
          audio::get(Device::Input).ok(),
        )
      });
      (
        devices,
        Some(audio::streams(player, music_only, music_mix)),
        None,
      )
    },
    move |(devices, streams, music)| {
      let mut state = model.borrow_mut();
      if state.audio_revision != revision
        || (!split_music && state.track.player_pid != player)
        || (state.panel == Panel::Music) != music_only
      {
        return;
      }
      let changed = devices.is_some_and(|(output, input)| {
        let changed = state.output != output || state.input != input;
        state.output = output;
        state.input = input;
        changed
      });
      let streams_changed = streams.is_some_and(|streams| state.set_streams(streams, music_only));
      let music_changed = music.is_some_and(|levels| {
        let changed = state.music_levels != levels;
        state.music_levels = levels;
        changed
      });
      drop(state);
      if changed || streams_changed || music_changed {
        refresh();
      }
    },
  );
}

fn background<T: Send + 'static>(
  work: impl FnOnce() -> T + Send + 'static,
  done: impl FnOnce(T) + 'static,
) {
  let (send, receive) = async_channel::bounded(1);
  thread::spawn(move || {
    let _ = send.send_blocking(work());
  });
  MainContext::default().spawn_local(async move {
    if let Ok(value) = receive.recv().await {
      done(value);
    }
  });
}

struct Display {
  desktop: RefCell<Desktop>,
  main_loop: MainLoop,
  failed: RefCell<Option<Box<dyn std::error::Error>>>,
  wants_write: Cell<bool>,
  animating: Cell<bool>,
  search_timer: RefCell<Option<(Instant, glib::SourceId)>>,
  model: Rc<RefCell<Model>>,
}

impl Display {
  fn draw(self: &Rc<Self>, changed: bool) {
    let result = {
      let mut desktop = self.desktop.borrow_mut();
      if changed {
        desktop.redraw()
      } else {
        desktop.draw_pending()
      }
    };
    self.update_search_timer();
    if !self.animating.get() && self.desktop.borrow_mut().animate_bar() {
      self.animating.set(true);
      let display = self.clone();
      glib::timeout_add_local(Duration::from_millis(16), move || {
        if display.desktop.borrow_mut().animate_bar() {
          display.draw(false);
        }
        if display.desktop.borrow_mut().animate_bar() {
          ControlFlow::Continue
        } else {
          display.animating.set(false);
          ControlFlow::Break
        }
      });
    }
    match result {
      Ok(false) if !self.wants_write.replace(true) => {
        let display = self.clone();
        let fd = display.desktop.borrow().fd();
        glib::unix_fd_add_local(fd, IOCondition::OUT, move |_, _| {
          match display.desktop.borrow().flush() {
            Ok(false) => ControlFlow::Continue,
            result => {
              display.wants_write.set(false);
              if let Err(error) = result {
                display.fail(error);
              }
              ControlFlow::Break
            }
          }
        });
      }
      Err(error) => self.fail(error),
      _ => {}
    }
  }

  fn update_search_timer(self: &Rc<Self>) {
    let deadline = self.desktop.borrow().search_deadline();
    let mut timer = self.search_timer.borrow_mut();
    if timer.as_ref().map(|(scheduled, _)| *scheduled) == deadline {
      return;
    }
    if let Some((_, source)) = timer.take() {
      source.remove();
    }
    if let Some(deadline) = deadline {
      let display = self.clone();
      let delay = deadline.saturating_duration_since(Instant::now());
      let source = glib::timeout_add_local_once(delay.max(Duration::from_millis(1)), move || {
        display.search_timer.borrow_mut().take();
        display.desktop.borrow_mut().tick_search();
        let query = {
          let mut state = display.model.borrow_mut();
          state
            .search_pending
            .filter(|due| *due <= Instant::now())
            .map(|_| {
              state.search_pending = None;
              state.search_query.trim().to_owned()
            })
        };
        if let Some(query) = query {
          let target = display.clone();
          let refresh: Rc<dyn Fn()> = Rc::new(move || target.draw(true));
          search_music(query, &display.model, &refresh);
        }
        display.draw(false);
      });
      *timer = Some((deadline, source));
    }
  }

  fn fail(&self, error: Box<dyn std::error::Error>) {
    *self.failed.borrow_mut() = Some(error);
    self.main_loop.quit();
  }
}

fn fetch_lyrics(model: &Rc<RefCell<Model>>, refresh: &Rc<dyn Fn()>) {
  let mut state = model.borrow_mut();
  let song = state.track.id.clone();
  if song.is_empty() {
    state.lyrics_id.clear();
    state.lyrics = Default::default();
    state.lyrics_status = Some("Play a song to see lyrics".into());
    drop(state);
    refresh();
    return;
  }
  let with_metadata = state.track.has_lyric_metadata();
  if state.lyrics_id == song && (state.lyrics_metadata || !with_metadata) {
    return;
  }
  state.lyrics_id.clone_from(&song);
  state.lyrics_metadata = with_metadata;
  state.lyrics = Default::default();
  state.lyrics_status = Some("Loading lyrics…".into());
  state.reset_music_scroll();
  drop(state);
  let model = model.clone();
  let refresh = refresh.clone();
  let request = song.clone();
  let (title, artist, duration) = {
    let state = model.borrow();
    (
      state.track.title.clone(),
      state.track.artist.clone(),
      state.track.duration,
    )
  };
  background(
    move || media::lyrics(&request, &title, &artist, duration).map_err(|error| error.to_string()),
    move |result| {
      let mut state = model.borrow_mut();
      if state.lyrics_id != song || state.lyrics_metadata != with_metadata {
        return;
      }
      match result {
        Ok(Some(lyrics)) if !lyrics.lines.is_empty() => {
          state.lyrics_status = None;
          state.lyrics = lyrics;
        }
        Ok(_) => state.lyrics_status = Some("No lyrics available".into()),
        Err(_) => {
          state.lyrics_id.clear();
          state.lyrics_status = Some("Could not load lyrics".into());
        }
      }
      drop(state);
      refresh();
    },
  );
}

fn search_music(query: String, model: &Rc<RefCell<Model>>, refresh: &Rc<dyn Fn()>) {
  let mut state = model.borrow_mut();
  if query.is_empty() || !state.signed_in || state.search_status.as_deref() == Some("Searching…")
  {
    return;
  }
  state.search_pending = None;
  let revision = state.search_revision;
  state.clear_search_focus();
  state.search_results.clear();
  state.search_status = Some("Searching…".into());
  state.reset_music_scroll();
  drop(state);
  refresh();
  let model = model.clone();
  let refresh = refresh.clone();
  let request = query.clone();
  background(
    move || media::search(&request).map_err(|error| error.to_string()),
    move |result| {
      let mut state = model.borrow_mut();
      if state.search_revision != revision || !state.signed_in {
        return;
      }
      match result {
        Ok(songs) => {
          state.search_status = songs.is_empty().then(|| "No songs found".into());
          state.search_results = songs;
        }
        Err(_) => state.search_status = Some("Search failed".into()),
      }
      drop(state);
      fetch_small_covers(&model, &refresh);
      refresh();
    },
  );
}

fn fetch_art(model: &Rc<RefCell<Model>>, refresh: &Rc<dyn Fn()>) {
  let mut state = model.borrow_mut();
  let url = state.cover_url();
  if state.panel != Panel::Music
    || !state.signed_in
    || url.is_empty()
    || state
      .large_cover
      .as_ref()
      .is_some_and(|(cached, _)| cached == url)
  {
    return;
  }
  let url = url.to_owned();
  state.large_cover = Some((url.clone(), None));
  drop(state);
  let model = model.clone();
  let refresh = refresh.clone();
  background(
    move || {
      let art = media::artwork(&url, 256);
      (url, art)
    },
    move |(url, art)| {
      let mut state = model.borrow_mut();
      if let Some((cached, cover)) = &mut state.large_cover
        && *cached == url
        && cover.is_none()
      {
        *cover = art.and_then(ui::image);
        drop(state);
        refresh();
      }
    },
  );
}

fn fetch_small_covers(model: &Rc<RefCell<Model>>, refresh: &Rc<dyn Fn()>) {
  let mut state = model.borrow_mut();
  if state.panel != Panel::Music || !state.signed_in {
    return;
  }
  let covers: Vec<_> = state
    .visible_cover_urls()
    .into_iter()
    .filter(|url| !url.is_empty())
    .map(str::to_owned)
    .collect();
  if state.small_covers.len() + covers.len() > 48 {
    state.small_covers.retain(|url, _| covers.contains(url));
  }
  for url in covers {
    if state.small_covers.contains_key(&url) {
      continue;
    }
    state.small_covers.insert(url.clone(), None);
    let model = model.clone();
    let refresh = refresh.clone();
    background(
      move || {
        let art = media::artwork(&url, 64);
        (url, art)
      },
      move |(url, art)| {
        let mut state = model.borrow_mut();
        if let Some(cover) = state.small_covers.get_mut(&url)
          && cover.is_none()
        {
          *cover = art.and_then(ui::image);
          drop(state);
          refresh();
        }
      },
    );
  }
}

fn watch_playlist(
  model: Rc<RefCell<Model>>,
  refresh: Rc<dyn Fn()>,
  socket: PathBuf,
  epoch: u64,
  delay: u32,
) {
  glib::timeout_add_seconds_local_once(delay, move || {
    if model.borrow().playlist_epoch != epoch {
      return;
    }
    refresh_active(&model, &refresh, &socket);
    watch_playlist(model, refresh, socket, epoch, 15);
  });
}

fn refresh_active(model: &Rc<RefCell<Model>>, refresh: &Rc<dyn Fn()>, socket: &Path) {
  let mut state = model.borrow_mut();
  if !state.signed_in || state.playlist_busy || state.playlist_syncing || !state.track.has_track {
    return;
  }
  let Some(playlist) = state.selected_playlist.clone() else {
    return;
  };
  let queue = state.track.queue.clone();
  if playlist.partial && (queue.is_empty() || !playlist.songs.contains(&state.track.id)) {
    return;
  }
  let epoch = state.playlist_epoch;
  state.playlist_syncing = true;
  drop(state);
  let socket = socket.to_owned();
  let model = model.clone();
  let refresh = refresh.clone();
  background(
    move || media::refresh_playlist(&socket, playlist, queue).map_err(|error| error.to_string()),
    move |result| {
      let mut state = model.borrow_mut();
      if state.playlist_epoch != epoch {
        return;
      }
      state.playlist_syncing = false;
      if let Ok(playlist) = result {
        let changed = state.selected_playlist.as_ref().is_none_or(|old| {
          old.name != playlist.name
            || old.songs != playlist.songs
            || old.partial != playlist.partial
        });
        if changed {
          state.selected_playlist = Some(playlist);
          media::save_playlist(state.selected_playlist.as_ref());
          drop(state);
          refresh();
        }
      }
    },
  );
}

fn initialize_wallpaper_environment() {
  let mut envp = Vec::new();
  for (mut key, value) in std::env::vars_os() {
    key.push(OsString::from("="));
    key.push(value);
    if let Ok(entry) = CString::new(key.as_os_str().as_bytes()) {
      envp.push(entry.into_raw().cast_const());
    }
  }
  envp.push(std::ptr::null());
  // The fork uses this immutable environment for Wayland and its FFmpeg children.
  unsafe { rustbg::set_envp(Box::leak(envp.into_boxed_slice()).as_ptr()) };
}
