use std::collections::{HashMap, HashSet};
use std::ffi::{CString, OsString, c_char, c_void};
use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use cairo::{Context, Format, ImageSurface};
use gio::glib::variant::ToVariant;
use gio::glib::{self, Variant};
use gio::prelude::*;
use gio::{Cancellable, DBusCallFlags, DesktopAppInfo};

use super::Haystack;
use crate::media::{Art, decode_art};

#[derive(PartialEq)]
pub struct App {
  pub id: String,
  pub path: PathBuf,
  pub name: String,
  pub detail: Option<String>,
  pub icon: Option<String>,
  pub search: Haystack,
}

/// Reads every visible desktop entry and renders the icons not in `known`.
pub fn load(size: i32, known: &HashSet<String>) -> (Vec<App>, Vec<(String, Option<Art>)>) {
  let mut roots = vec![glib::user_data_dir()];
  roots.extend(glib::system_data_dirs());
  let mut seen = HashSet::new();
  let mut apps = Vec::new();
  for root in &roots {
    scan(&root.join("applications"), "", &mut seen, &mut apps);
  }
  apps.sort_by_cached_key(|app| app.name.to_lowercase());
  let index = Icons::index(&roots);
  let mut rendered = HashSet::new();
  let icons = apps
    .iter()
    .filter_map(|app| app.icon.clone())
    .filter(|icon| !known.contains(icon) && rendered.insert(icon.clone()))
    .map(|icon| {
      let art = index.render(&icon, size);
      (icon, art)
    })
    .collect();
  (apps, icons)
}

// Earlier data directories shadow later ones by desktop ID, including hidden entries.
fn scan(dir: &Path, prefix: &str, seen: &mut HashSet<String>, apps: &mut Vec<App>) {
  let Ok(entries) = std::fs::read_dir(dir) else {
    return;
  };
  let mut entries: Vec<_> = entries.flatten().collect();
  entries.sort_by_key(std::fs::DirEntry::file_name);
  for entry in entries {
    let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
      continue;
    };
    let path = entry.path();
    if path.is_dir() {
      scan(&path, &format!("{prefix}{name}-"), seen, apps);
      continue;
    }
    if !name.ends_with(".desktop") || !seen.insert(format!("{prefix}{name}")) {
      continue;
    }
    // GIO rejects other entry types and missing TryExec programs.
    let Some(info) = DesktopAppInfo::from_filename(&path) else {
      continue;
    };
    if info.is_hidden() || !info.should_show() {
      continue;
    }
    let name = info.name().to_string();
    let generic = info.generic_name().map(String::from);
    let comment = info.description().map(String::from);
    let keywords = info.keywords().join(" ");
    let program = info
      .executable()
      .file_name()
      .map(|name| name.to_string_lossy().into_owned())
      .unwrap_or_default();
    let search = Haystack::new([
      (name.as_str(), 4, true),
      (generic.as_deref().unwrap_or_default(), 2, true),
      (keywords.as_str(), 2, true),
      (program.as_str(), 1, true),
      (comment.as_deref().unwrap_or_default(), 1, false),
    ]);
    apps.push(App {
      id: format!("{prefix}{}", entry.file_name().to_string_lossy()),
      path,
      name,
      detail: generic.or(comment).filter(|detail| !detail.is_empty()),
      icon: info
        .string("Icon")
        .map(|icon| icon.trim().to_owned())
        .filter(|icon| !icon.is_empty()),
      search,
    });
  }
}

/// Icons from the hicolor theme and pixmaps, keyed by icon name.
struct Icons(HashMap<String, Vec<(Option<i32>, PathBuf)>>);

impl Icons {
  fn index(roots: &[PathBuf]) -> Self {
    let mut icons: HashMap<String, Vec<_>> = HashMap::new();
    let mut add = |dir: &Path, size: Option<i32>| {
      for file in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = file.path();
        if let Some(name) = path.file_stem().and_then(|name| name.to_str())
          && matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("png" | "svg")
          )
        {
          icons.entry(name.to_owned()).or_default().push((size, path));
        }
      }
    };
    for root in roots {
      for sized in read_dirs(&root.join("icons/hicolor")) {
        let name = sized
          .file_name()
          .and_then(|name| name.to_str())
          .unwrap_or_default();
        let (base, factor) = name.split_once('@').unwrap_or((name, "1"));
        let pixels = base
          .split_once('x')
          .and_then(|(width, _)| width.parse::<i32>().ok())
          .zip(factor.parse::<i32>().ok())
          .map(|(size, factor)| size * factor);
        for context in read_dirs(&sized) {
          add(&context, pixels.or(Some(0)));
        }
      }
    }
    for root in roots {
      add(&root.join("pixmaps"), None);
    }
    Self(icons)
  }

  fn render(&self, icon: &str, size: i32) -> Option<Art> {
    let path = if icon.starts_with('/') {
      PathBuf::from(icon)
    } else {
      let name = [".png", ".svg", ".xpm"]
        .into_iter()
        .find_map(|ext| icon.strip_suffix(ext))
        .unwrap_or(icon);
      // Prefer an exact bitmap, then a vector, then the nearest larger bitmap.
      let rank = |(pixels, path): &&(Option<i32>, PathBuf)| {
        let vector = path.extension().is_some_and(|ext| ext == "svg");
        match pixels {
          Some(pixels) if !vector && *pixels == size => 0,
          Some(_) if vector => 1,
          Some(pixels) if *pixels > size => 2 + pixels - size,
          Some(pixels) => 10_000 + size - pixels,
          None => 20_000,
        }
      };
      self.0.get(name)?.iter().min_by_key(rank)?.1.clone()
    };
    if path.extension().is_some_and(|ext| ext == "svg") {
      render_svg(&path, size)
    } else {
      decode_art(std::fs::read(path).ok()?, size as u32)
    }
  }
}

fn read_dirs(dir: &Path) -> impl Iterator<Item = PathBuf> {
  std::fs::read_dir(dir)
    .into_iter()
    .flatten()
    .flatten()
    .map(|entry| entry.path())
    .filter(|path| path.is_dir())
}

#[repr(C)]
struct Rectangle {
  x: f64,
  y: f64,
  width: f64,
  height: f64,
}

#[link(name = "rsvg-2")]
unsafe extern "C" {
  fn rsvg_handle_new_from_file(
    path: *const c_char,
    error: *mut *mut glib::ffi::GError,
  ) -> *mut c_void;
  fn rsvg_handle_render_document(
    handle: *mut c_void,
    cr: *mut cairo::ffi::cairo_t,
    viewport: *const Rectangle,
    error: *mut *mut glib::ffi::GError,
  ) -> glib::ffi::gboolean;
}

fn render_svg(path: &Path, size: i32) -> Option<Art> {
  let path = CString::new(path.as_os_str().as_bytes()).ok()?;
  let mut surface = ImageSurface::create(Format::ARgb32, size, size).ok()?;
  // SAFETY: The path is NUL-terminated and errors may be ignored by passing null.
  let handle = unsafe { rsvg_handle_new_from_file(path.as_ptr(), std::ptr::null_mut()) };
  if handle.is_null() {
    return None;
  }
  let viewport = Rectangle {
    x: 0.0,
    y: 0.0,
    width: f64::from(size),
    height: f64::from(size),
  };
  let rendered = Context::new(&surface).is_ok_and(|cr| {
    // SAFETY: The handle is a live RsvgHandle and the context outlives the call.
    unsafe {
      rsvg_handle_render_document(handle, cr.to_raw_none(), &viewport, std::ptr::null_mut()) != 0
    }
  });
  // SAFETY: The handle was returned with a reference owned by this function.
  unsafe { glib::gobject_ffi::g_object_unref(handle.cast()) };
  if !rendered {
    return None;
  }
  surface.flush();
  Some(Art {
    data: surface.data().ok()?.to_vec(),
    width: size,
    height: size,
  })
}

pub fn launch(app: &App) -> Result<(), String> {
  let info = DesktopAppInfo::from_filename(&app.path)
    .ok_or_else(|| format!("{} is no longer installed", app.name))?;
  let id = app.id.trim_end_matches(".desktop");
  if info.boolean("Terminal")
    && let Some(argv) = terminal_argv(&info, &app.path)
  {
    let mut command = Command::new(&argv[0]);
    command
      .args(&argv[1..])
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null());
    if let Some(dir) = info.string("Path").filter(|dir| !dir.is_empty()) {
      command.current_dir(dir.as_str());
    }
    // SAFETY: setsid is async-signal-safe and runs in the forked child.
    unsafe {
      command.pre_exec(|| {
        libc::setsid();
        Ok(())
      });
    }
    let pid = command.spawn().map_err(|error| error.to_string())?.id() as i32;
    start_scope(id, pid);
    // Reaps the terminal when it exits.
    glib::child_watch_add_local(glib::Pid(pid), |_, _| {});
    return Ok(());
  }
  let null = File::options()
    .read(true)
    .write(true)
    .open("/dev/null")
    .map_err(|error| error.to_string())?;
  info
    .launch_uris_as_manager_with_fds(
      &[],
      None::<&gio::AppLaunchContext>,
      glib::SpawnFlags::SEARCH_PATH,
      Some(Box::new(|| {
        // SAFETY: setsid is async-signal-safe and runs in the forked child.
        unsafe { libc::setsid() };
      })),
      Some(&mut |_, pid| start_scope(id, pid.0)),
      Some(&null),
      Some(&null),
      Some(&null),
    )
    .map_err(|error| error.to_string())
}

/// GIO only knows a fixed list of terminals, so `$TERMINAL` and `xdg-terminal-exec` come first.
fn terminal_argv(info: &DesktopAppInfo, path: &Path) -> Option<Vec<OsString>> {
  let mut argv = match std::env::var_os("TERMINAL")
    .filter(|terminal| !terminal.is_empty())
    .and_then(glib::find_program_in_path)
  {
    Some(terminal) => vec![terminal.into(), "-e".into()],
    None => vec![glib::find_program_in_path("xdg-terminal-exec")?.into()],
  };
  for arg in glib::shell_parse_argv(info.string("Exec")?.as_str()).ok()? {
    let arg = arg.into_string().ok()?;
    if arg == "%i" {
      if let Some(icon) = info.string("Icon") {
        argv.extend(["--icon".into(), icon.into()]);
      }
      continue;
    }
    // File and URL codes expand to nothing because no files are opened.
    let mut expanded = String::new();
    let mut chars = arg.chars();
    while let Some(character) = chars.next() {
      if character != '%' {
        expanded.push(character);
        continue;
      }
      match chars.next() {
        Some('%') => expanded.push('%'),
        Some('c') => expanded.push_str(&info.name()),
        Some('k') => expanded.push_str(&path.to_string_lossy()),
        _ => {}
      }
    }
    if !expanded.is_empty() || arg.is_empty() {
      argv.push(expanded.into());
    }
  }
  Some(argv)
}

// Applications would otherwise share the Lintel service cgroup and stop with it.
fn start_scope(id: &str, pid: i32) {
  let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, None::<&Cancellable>) else {
    return;
  };
  let mut escaped = String::new();
  for byte in id.bytes() {
    if byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'.') {
      escaped.push(char::from(byte));
    } else {
      escaped.push_str(&format!("\\x{byte:02x}"));
    }
  }
  let properties = vec![
    ("PIDs".to_owned(), vec![pid as u32].to_variant()),
    ("CollectMode".to_owned(), "inactive-or-failed".to_variant()),
  ];
  let auxiliary: Vec<(String, Vec<(String, Variant)>)> = Vec::new();
  bus.call(
    Some("org.freedesktop.systemd1"),
    "/org/freedesktop/systemd1",
    "org.freedesktop.systemd1.Manager",
    "StartTransientUnit",
    Some(
      &(
        format!("app-lintel-{escaped}-{pid}.scope"),
        "fail",
        properties,
        auxiliary,
      )
        .to_variant(),
    ),
    None,
    DBusCallFlags::NONE,
    -1,
    None::<&Cancellable>,
    |_| {},
  );
}
