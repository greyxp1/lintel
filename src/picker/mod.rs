mod apps;
mod clipboard;
mod paint;
mod vault;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::process::{Command as Process, Stdio};
use std::rc::Rc;
use std::sync::mpsc;

use cairo::ImageSurface;
use xkbcommon::xkb::keysyms;

pub use paint::{HEIGHT, RADIUS, WIDTH, paint};

use crate::background;
use crate::services::Services;
use crate::ui::{Model, Notice, image};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
  Apps,
  Clipboard,
  Passwords,
}

impl Mode {
  pub fn parse(name: &str) -> Option<Self> {
    match name {
      "apps" => Some(Self::Apps),
      "clipboard" => Some(Self::Clipboard),
      "passwords" => Some(Self::Passwords),
      _ => None,
    }
  }
}

pub enum Command {
  Activate,
  Delete,
  Thumbnails,
  /// Renders application icons and thumbnails again for a new output scale.
  Scale,
  /// The compositor has processed the picker unmap.
  Unmapped,
}

pub struct Key<'a> {
  pub sym: u32,
  pub text: &'a str,
  pub ctrl: bool,
  pub alt: bool,
}

enum Step {
  List,
  /// The chosen login with the list query and selection to restore.
  Actions(vault::Login, String, usize),
  Wipe,
}

#[derive(PartialEq)]
enum Status {
  Ready,
  Loading,
  Unlocking,
  Failed(String),
}

struct View {
  mode: Mode,
  initial: String,
  query: String,
  matches: Vec<usize>,
  selected: usize,
  offset: usize,
  hover: Option<usize>,
  scroll: f64,
  step: Step,
  status: Status,
}

#[derive(Default)]
pub struct Picker {
  view: Option<View>,
  session: u64,
  pub scale: i32,
  apps: Vec<apps::App>,
  apps_loading: bool,
  apps_stale: bool,
  icons: HashMap<String, Option<ImageSurface>>,
  icon_size: i32,
  clips: Vec<clipboard::Clip>,
  clips_loaded: bool,
  /// Bumped by every refresh and removal so older listings are discarded.
  clipboard_gen: u64,
  thumbs: HashMap<u64, Option<ImageSurface>>,
  thumbs_pending: HashSet<u64>,
  thumbs_scale: i32,
  logins: Vec<vault::Login>,
}

impl Picker {
  /// The picker stays open but unmapped while pinentry asks for the vault password.
  pub fn visible(&self) -> bool {
    self
      .view
      .as_ref()
      .is_some_and(|view| view.status != Status::Unlocking)
  }

  pub fn close(&mut self) {
    self.view = None;
    self.session += 1;
  }

  fn open(&mut self, mode: Mode, search: &str) {
    let ready = match mode {
      Mode::Apps => !self.apps.is_empty(),
      Mode::Clipboard => self.clips_loaded,
      Mode::Passwords => false,
    };
    if mode == Mode::Passwords {
      self.logins.clear();
    }
    self.session += 1;
    self.view = Some(View {
      mode,
      initial: search.to_owned(),
      query: search.to_owned(),
      matches: Vec::new(),
      selected: 0,
      offset: 0,
      hover: None,
      scroll: 0.0,
      step: Step::List,
      status: if ready {
        Status::Ready
      } else {
        Status::Loading
      },
    });
    self.refilter(false);
  }

  fn set_status(&mut self, status: Status) {
    if let Some(view) = &mut self.view {
      view.status = status;
    }
    self.refilter(true);
  }

  fn showing(&self, mode: Mode) -> bool {
    self.view.as_ref().is_some_and(|view| view.mode == mode)
  }

  fn refilter(&mut self, keep: bool) {
    let Some(view) = &mut self.view else {
      return;
    };
    let actions;
    let haystacks: Vec<&Haystack> = match (&view.step, view.mode) {
      (Step::Wipe, _) => Vec::new(),
      (Step::Actions(..), _) => {
        actions = vault::ACTIONS.map(|(_, name)| Haystack::new([(name, 1, true)]));
        actions.iter().collect()
      }
      // Entry names stay hidden until the vault is known to be unlocked.
      (_, Mode::Passwords) if view.status != Status::Ready => Vec::new(),
      (_, Mode::Apps) => self.apps.iter().map(|app| &app.search).collect(),
      (_, Mode::Clipboard) => self.clips.iter().map(|clip| &clip.search).collect(),
      (_, Mode::Passwords) => self.logins.iter().map(|login| &login.search).collect(),
    };
    let tokens: Vec<String> = view
      .query
      .to_lowercase()
      .split_whitespace()
      .map(str::to_owned)
      .collect();
    let mut scored: Vec<(i32, usize)> = haystacks
      .into_iter()
      .enumerate()
      .filter_map(|(index, haystack)| Some((haystack.score(&tokens)?, index)))
      .collect();
    scored.sort_by_key(|&(score, index)| (std::cmp::Reverse(score), index));
    view.matches = scored.into_iter().map(|(_, index)| index).collect();
    view.hover = None;
    if keep {
      view.select(view.selected as isize, paint::rows(view));
    } else {
      view.selected = 0;
      view.offset = 0;
    }
  }

  fn back(&mut self) {
    let Some(view) = &mut self.view else {
      return;
    };
    match std::mem::replace(&mut view.step, Step::List) {
      Step::List => self.close(),
      Step::Actions(_, query, selected) => {
        view.query = query;
        self.refilter(false);
        if let Some(view) = &mut self.view {
          view.select(selected as isize, paint::rows(view));
        }
      }
      Step::Wipe => self.refilter(true),
    }
  }

  fn edit(&mut self, edit: impl FnOnce(&mut String)) -> bool {
    let Some(view) = self
      .view
      .as_mut()
      .filter(|view| !matches!(view.step, Step::Wipe))
    else {
      return false;
    };
    let previous = view.query.len();
    edit(&mut view.query);
    let changed = view.query.len() != previous;
    if changed {
      self.refilter(false);
    }
    changed
  }

  pub fn key(&mut self, key: Key) -> (bool, Option<Command>) {
    let Some(view) = &mut self.view else {
      return (false, None);
    };
    let rows = paint::rows(view) as isize;
    let list = matches!(view.step, Step::List);
    let clipboard = list && view.mode == Mode::Clipboard;
    let movement = match key.sym {
      keysyms::KEY_Up | keysyms::KEY_ISO_Left_Tab => Some(-1),
      keysyms::KEY_Down | keysyms::KEY_Tab => Some(1),
      keysyms::KEY_p | keysyms::KEY_k if key.ctrl => Some(-1),
      keysyms::KEY_n | keysyms::KEY_j if key.ctrl => Some(1),
      keysyms::KEY_Page_Up => Some(-rows),
      keysyms::KEY_Page_Down => Some(rows),
      _ => None,
    };
    let changed = if let Some(delta) = movement {
      let previous = (view.selected, view.offset);
      view.select(view.selected as isize + delta, rows as usize);
      previous != (view.selected, view.offset)
    } else {
      match key.sym {
        keysyms::KEY_Escape => {
          self.back();
          true
        }
        keysyms::KEY_Return | keysyms::KEY_KP_Enter => return (false, Some(Command::Activate)),
        keysyms::KEY_BackSpace if view.query.is_empty() && !list => {
          self.back();
          true
        }
        keysyms::KEY_BackSpace => self.edit(|query| {
          query.pop();
        }),
        keysyms::KEY_w if key.ctrl => self.edit(|query| {
          let end = query.trim_end().len();
          let start = query[..end]
            .rfind(char::is_whitespace)
            .map_or(0, |index| index + 1);
          query.truncate(start);
        }),
        keysyms::KEY_u if key.ctrl => self.edit(String::clear),
        keysyms::KEY_1 if key.alt && clipboard => return (false, Some(Command::Delete)),
        keysyms::KEY_0 if key.alt && clipboard && !self.clips.is_empty() => {
          view.step = Step::Wipe;
          view.hover = None;
          true
        }
        _ if !key.ctrl
          && !key.alt
          && !key.text.is_empty()
          && !key.text.chars().any(char::is_control) =>
        {
          self.edit(|query| {
            if query.len() < 256 {
              query.push_str(key.text);
            }
          })
        }
        _ => false,
      }
    };
    let thumbnails = changed && self.showing(Mode::Clipboard);
    (changed, thumbnails.then_some(Command::Thumbnails))
  }

  pub fn hover(&mut self, pointer: Option<(f64, f64)>) -> bool {
    let Some(view) = &mut self.view else {
      return false;
    };
    let hover = pointer.and_then(|(x, y)| paint::row_at(view, x, y));
    std::mem::replace(&mut view.hover, hover) != hover
  }

  pub fn click(&mut self, x: f64, y: f64) -> Option<Command> {
    let view = self.view.as_mut()?;
    if matches!(view.step, Step::Wipe) {
      return match paint::wipe_button(x, y)? {
        true => Some(Command::Activate),
        false => {
          self.back();
          None
        }
      };
    }
    view.selected = paint::row_at(view, x, y)?;
    Some(Command::Activate)
  }

  pub fn scroll(&mut self, amount: f64) -> bool {
    let Some(view) = &mut self.view else {
      return false;
    };
    // Wheel notches arrive as 15 units, touchpads as many smaller steps.
    view.scroll += amount;
    let rows = (view.scroll / 15.0).trunc();
    view.scroll -= rows * 15.0;
    let visible = paint::rows(view);
    let last = view.matches.len().saturating_sub(visible);
    let offset = (view.offset as isize + rows as isize).clamp(0, last as isize) as usize;
    if offset == view.offset {
      return false;
    }
    view.offset = offset;
    view.selected = view.selected.clamp(offset, offset + visible - 1);
    true
  }
}

impl View {
  fn select(&mut self, index: isize, rows: usize) {
    let last = self.matches.len().saturating_sub(1);
    self.selected = index.clamp(0, last as isize) as usize;
    if self.selected < self.offset {
      self.offset = self.selected;
    } else if self.selected >= self.offset + rows {
      self.offset = self.selected + 1 - rows;
    }
    self.offset = self.offset.min(self.matches.len().saturating_sub(rows));
  }
}

/// Lowercase search fields with a weight and whether they allow scattered matches.
#[derive(Clone, PartialEq)]
pub struct Haystack(Vec<(String, i32, bool)>);

impl Haystack {
  fn new<'a>(fields: impl IntoIterator<Item = (&'a str, i32, bool)>) -> Self {
    Self(
      fields
        .into_iter()
        .filter(|(text, ..)| !text.is_empty())
        .map(|(text, weight, fuzzy)| (text.to_lowercase(), weight, fuzzy))
        .collect(),
    )
  }

  /// Every token must match some field. Exact, prefix and word matches rank first.
  fn score(&self, tokens: &[String]) -> Option<i32> {
    tokens
      .iter()
      .map(|token| {
        self
          .0
          .iter()
          .filter_map(|(text, weight, fuzzy)| Some(field_score(token, text, *fuzzy)? * weight))
          .max()
      })
      .sum()
  }
}

fn field_score(token: &str, text: &str, fuzzy: bool) -> Option<i32> {
  if let Some(position) = text.find(token) {
    let word = text[..position]
      .chars()
      .next_back()
      .is_none_or(|previous| !previous.is_alphanumeric());
    let bonus = match () {
      () if text.len() == token.len() => 3000,
      () if position == 0 => 2000,
      () if word => 1000,
      () => 0,
    };
    return Some(1000 + bonus - (position as i32).min(500));
  }
  if !fuzzy {
    return None;
  }
  let wanted: Vec<char> = token.chars().collect();
  let (mut matched, mut score, mut first, mut last) = (0, 0, None, 0);
  let mut previous = None;
  for (index, character) in text.chars().enumerate() {
    if wanted.get(matched) == Some(&character) {
      score += 10;
      if previous.is_none_or(|previous: char| !previous.is_alphanumeric()) {
        score += 15;
      }
      if matched > 0 && last + 1 == index {
        score += 20;
      }
      first.get_or_insert(index);
      last = index;
      matched += 1;
    }
    previous = Some(character);
  }
  let span = last - first?;
  // Letters scattered across a long field are rarely what was meant.
  (matched == wanted.len() && span < wanted.len() * 3 + 2)
    .then(|| score - (span + 1 - wanted.len()) as i32)
}

fn run(program: &str, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>, String> {
  let mut child = Process::new(program)
    .args(args)
    .stdin(if input.is_some() {
      Stdio::piped()
    } else {
      Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .map_err(|error| format!("Could not run {program}: {error}"))?;
  if let Some(input) = input
    && let Some(mut stdin) = child.stdin.take()
  {
    let _ = stdin.write_all(input);
  }
  let output = child
    .wait_with_output()
    .map_err(|error| error.to_string())?;
  if output.status.success() {
    return Ok(output.stdout);
  }
  let error = String::from_utf8_lossy(&output.stderr);
  Err(
    error
      .lines()
      .rev()
      .map(str::trim)
      .find(|line| !line.is_empty())
      .map_or_else(|| format!("{program} failed"), str::to_owned),
  )
}

fn copy(data: &[u8], trim: bool) -> Result<(), String> {
  // The forked clipboard server would otherwise hold output pipes open.
  let mut child = Process::new("wl-copy")
    .args(trim.then_some("--trim-newline"))
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .map_err(|error| format!("Could not run wl-copy: {error}"))?;
  if let Some(mut stdin) = child.stdin.take() {
    let _ = stdin.write_all(data);
  }
  match child.wait() {
    Ok(status) if status.success() => Ok(()),
    _ => Err("Could not copy to the clipboard".into()),
  }
}

pub struct Controller {
  model: Rc<RefCell<Model>>,
  refresh: Rc<dyn Fn()>,
  services: Rc<Services>,
  /// Releases a vault action waiting for keyboard focus to leave the picker.
  unmapped: RefCell<Option<mpsc::Sender<()>>>,
}

impl Controller {
  pub fn new(model: Rc<RefCell<Model>>, refresh: Rc<dyn Fn()>, services: Rc<Services>) -> Rc<Self> {
    Rc::new(Self {
      model,
      refresh,
      services,
      unmapped: RefCell::default(),
    })
  }

  pub fn toggle(self: &Rc<Self>, mode: Mode, search: &str) {
    let mut model = self.model.borrow_mut();
    let picker = &mut model.picker;
    let opened = match &mut picker.view {
      Some(view) if view.mode == mode => {
        if search.is_empty() || search == view.initial {
          picker.close();
        } else {
          view.initial = search.to_owned();
          view.query = search.to_owned();
          view.step = Step::List;
          picker.refilter(false);
        }
        false
      }
      _ => {
        picker.open(mode, search);
        true
      }
    };
    drop(model);
    if opened {
      match mode {
        Mode::Apps => self.load_apps(),
        Mode::Clipboard => self.load_clips(),
        Mode::Passwords => self.load_logins(),
      }
    }
    (self.refresh)();
    self.thumbnails();
  }

  pub fn run(self: &Rc<Self>, command: Command) {
    match command {
      Command::Activate => self.activate(),
      Command::Delete => self.delete(),
      Command::Thumbnails => self.thumbnails(),
      Command::Scale => {
        self.load_apps();
        self.thumbnails();
      }
      Command::Unmapped => {
        if let Some(unmapped) = self.unmapped.take() {
          let _ = unmapped.send(());
        }
      }
    }
  }

  fn activate(self: &Rc<Self>) {
    let mut model = self.model.borrow_mut();
    let picker = &mut model.picker;
    let Some(view) = &mut picker.view else {
      return;
    };
    let selected = view.matches.get(view.selected).copied();
    match (&view.step, view.mode) {
      (Step::Wipe, _) => {
        picker.clips.clear();
        picker.clipboard_gen += 1;
        picker.thumbs.clear();
        picker.back();
        drop(model);
        (self.refresh)();
        let controller = self.clone();
        background(clipboard::wipe, move |result| {
          if let Err(error) = result {
            controller.notify("Could not wipe clipboard history", error);
            controller.load_clips();
          }
        });
      }
      (Step::Actions(login, ..), _) => {
        let Some(index) = selected else {
          return;
        };
        let (action, id) = (vault::ACTIONS[index].0, login.id.clone());
        picker.close();
        drop(model);
        let (unmapped, wait) = mpsc::channel();
        self.unmapped.replace(Some(unmapped));
        (self.refresh)();
        let controller = self.clone();
        background(
          move || vault::perform(action, &id, &wait),
          move |result| {
            if let Err(error) = result {
              controller.notify("Password picker", error);
            }
          },
        );
      }
      (Step::List, Mode::Apps) => {
        let Some(app) = selected.and_then(|index| picker.apps.get(index)) else {
          return;
        };
        let result = apps::launch(app);
        picker.close();
        drop(model);
        (self.refresh)();
        if let Err(error) = result {
          self.notify("Could not launch application", error);
        }
      }
      (Step::List, Mode::Clipboard) => {
        let Some(id) = selected
          .and_then(|index| picker.clips.get(index))
          .map(|clip| clip.id)
        else {
          return;
        };
        picker.close();
        drop(model);
        (self.refresh)();
        let controller = self.clone();
        background(
          move || clipboard::select(id),
          move |result| {
            if let Err(error) = result {
              controller.notify("Could not copy clipboard entry", error);
            }
          },
        );
      }
      (Step::List, Mode::Passwords) => {
        let Some(login) = selected.and_then(|index| picker.logins.get(index)) else {
          return;
        };
        view.step = Step::Actions(
          login.clone(),
          std::mem::take(&mut view.query),
          view.selected,
        );
        picker.refilter(false);
        drop(model);
        (self.refresh)();
      }
    }
  }

  fn delete(self: &Rc<Self>) {
    let mut model = self.model.borrow_mut();
    let picker = &mut model.picker;
    let Some(index) = picker
      .view
      .as_ref()
      .filter(|view| view.mode == Mode::Clipboard && matches!(view.step, Step::List))
      .and_then(|view| view.matches.get(view.selected).copied())
    else {
      return;
    };
    let id = picker.clips.remove(index).id;
    picker.clipboard_gen += 1;
    picker.thumbs.remove(&id);
    picker.refilter(true);
    drop(model);
    (self.refresh)();
    self.thumbnails();
    let controller = self.clone();
    background(
      move || clipboard::delete(id),
      move |result| {
        if let Err(error) = result {
          controller.notify("Could not delete clipboard entry", error);
          controller.load_clips();
        }
      },
    );
  }

  fn load_apps(self: &Rc<Self>) {
    let mut model = self.model.borrow_mut();
    let picker = &mut model.picker;
    if std::mem::replace(&mut picker.apps_loading, true) {
      picker.apps_stale = true;
      return;
    }
    let size = paint::ICON * picker.scale.max(1);
    if picker.icon_size != size {
      picker.icons.clear();
      picker.icon_size = size;
    }
    let known: HashSet<String> = picker.icons.keys().cloned().collect();
    drop(model);
    let controller = self.clone();
    background(
      move || apps::load(size, &known),
      move |(apps, icons)| {
        let mut model = controller.model.borrow_mut();
        let picker = &mut model.picker;
        picker.apps_loading = false;
        if picker.icon_size == size {
          picker.icons.extend(
            icons
              .into_iter()
              .map(|(name, art)| (name, art.and_then(image))),
          );
        }
        if picker.apps != apps {
          picker.apps = apps;
          picker.refilter(true);
        }
        if picker.showing(Mode::Apps) {
          picker.set_status(Status::Ready);
        }
        let again = std::mem::take(&mut picker.apps_stale);
        drop(model);
        (controller.refresh)();
        if again {
          controller.load_apps();
        }
      },
    );
  }

  fn load_clips(self: &Rc<Self>) {
    let generation = {
      let picker = &mut self.model.borrow_mut().picker;
      picker.clipboard_gen += 1;
      picker.clipboard_gen
    };
    let controller = self.clone();
    background(clipboard::list, move |result| {
      let mut model = controller.model.borrow_mut();
      let picker = &mut model.picker;
      if picker.clipboard_gen != generation {
        return;
      }
      let showing = picker.showing(Mode::Clipboard);
      match result {
        Ok(clips) => {
          let ids: HashSet<u64> = clips.iter().map(|clip| clip.id).collect();
          picker.thumbs.retain(|id, _| ids.contains(id));
          picker.clips = clips;
          picker.clips_loaded = true;
          if showing {
            picker.set_status(Status::Ready);
          }
        }
        Err(error) if showing => picker.set_status(Status::Failed(error)),
        Err(_) => {}
      }
      drop(model);
      (controller.refresh)();
      controller.thumbnails();
    });
  }

  fn load_logins(self: &Rc<Self>) {
    let session = self.model.borrow().picker.session;
    let controller = self.clone();
    background(vault::unlocked, move |unlocked| {
      if unlocked {
        controller.list_logins(session);
        return;
      }
      let locked = controller.update(session, |picker| picker.set_status(Status::Unlocking));
      if !locked {
        return;
      }
      let controller = controller.clone();
      background(vault::unlock, move |result| match result {
        Ok(()) => controller.list_logins(session),
        Err(error) => {
          controller.update(session, |picker| {
            picker.set_status(Status::Failed(format!(
              "Could not unlock the vault: {error}"
            )));
          });
        }
      });
    });
  }

  /// Shows the cached list first and replaces it once `rbw sync` finishes.
  fn list_logins(self: &Rc<Self>, session: u64) {
    // Whether the synced list was applied, so the cached one cannot override it.
    let synced = Rc::new(Cell::new(false));
    for sync in [false, true] {
      let controller = self.clone();
      let synced = synced.clone();
      background(
        move || {
          if sync {
            vault::sync();
          }
          vault::list()
        },
        move |result| {
          if synced.get() {
            return;
          }
          controller.update(session, |picker| match result {
            Ok(logins) => {
              synced.set(sync);
              picker.logins = logins;
              picker.set_status(Status::Ready);
            }
            Err(error) if !sync => {
              if picker.view.as_ref().is_some_and(|view| view.status != Status::Ready) {
                picker.set_status(Status::Failed(error));
              }
            }
            Err(_) => {}
          });
        },
      );
    }
  }

  /// Applies a vault result only while the picker session that requested it is open.
  fn update(&self, session: u64, change: impl FnOnce(&mut Picker)) -> bool {
    let mut model = self.model.borrow_mut();
    if model.picker.session != session || !model.picker.showing(Mode::Passwords) {
      return false;
    }
    change(&mut model.picker);
    drop(model);
    (self.refresh)();
    true
  }

  fn thumbnails(self: &Rc<Self>) {
    let mut model = self.model.borrow_mut();
    let picker = &mut model.picker;
    let scale = picker.scale.max(1);
    if picker.thumbs_scale != scale {
      picker.thumbs.clear();
      picker.thumbs_pending.clear();
      picker.thumbs_scale = scale;
    }
    let Some(view) = picker
      .view
      .as_ref()
      .filter(|view| view.mode == Mode::Clipboard && matches!(view.step, Step::List))
    else {
      return;
    };
    let wanted: Vec<u64> = view
      .matches
      .iter()
      .skip(view.offset)
      .take(paint::rows(view))
      .filter_map(|&index| picker.clips.get(index))
      .filter(|clip| {
        clip.image.is_some()
          && !picker.thumbs.contains_key(&clip.id)
          && !picker.thumbs_pending.contains(&clip.id)
      })
      .map(|clip| clip.id)
      .collect();
    if wanted.is_empty() {
      return;
    }
    picker.thumbs_pending.extend(&wanted);
    let (width, height) = paint::thumbnail_size(scale);
    drop(model);
    let controller = self.clone();
    background(
      move || {
        wanted
          .into_iter()
          .map(|id| (id, clipboard::thumbnail(id, width, height)))
          .collect::<Vec<_>>()
      },
      move |thumbnails| {
        let mut model = controller.model.borrow_mut();
        let picker = &mut model.picker;
        if picker.thumbs_scale != scale {
          return;
        }
        for (id, art) in thumbnails {
          picker.thumbs_pending.remove(&id);
          picker.thumbs.insert(id, art.and_then(image));
        }
        drop(model);
        (controller.refresh)();
      },
    );
  }

  fn notify(&self, summary: &str, body: String) {
    self.services.notify_local(
      "picker".into(),
      Notice {
        summary: summary.into(),
        body,
        ..Notice::default()
      },
      |_| {},
    );
  }
}
