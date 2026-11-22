#![no_std]

#[cfg(feature = "standalone")]
pub mod cli;
mod env;
mod error;
mod fmt_lite;
mod image;
mod io;
mod process;
mod wayland;

use core::ffi::c_int;

pub use env::set_envp;
use error::write_wire_diagnostic;
use fmt_lite::StringOnStack;
use wayland::State;
use wllib::dispatch::dispatch_once;
use wllib::error::WireError::ConnectionClosed;
use wllib::registry::crawl;

pub struct Config {
  image_path: StringOnStack<256>,
  namespace: StringOnStack<256>,
  blur: u32,
}

impl Default for Config {
  fn default() -> Self {
    Self {
      image_path: StringOnStack::new(),
      namespace: StringOnStack::from("wallpaper"),
      blur: 0,
    }
  }
}

impl Config {
  pub fn new(image: &str, namespace: &str, blur: u32) -> Option<Self> {
    if image.is_empty()
      || image.len() > 255
      || image.contains('\0')
      || namespace.is_empty()
      || namespace.len() > 227
      || namespace.contains('\0')
      || blur > 1024
    {
      return None;
    }
    Some(Self {
      image_path: StringOnStack::from(image),
      namespace: StringOnStack::from(namespace),
      blur,
    })
  }
}

/// Serve a wallpaper until the compositor disconnects. Initialize the process environment
/// with [set_envp] before calling this function.
pub fn run(config: Config) -> c_int {
  let mut conn = match env::connect_wayland() {
    Ok(c) => c,
    Err(e) => {
      write_wire_diagnostic("[rustbg]", &e);
      return 1;
    }
  };

  let mut state = State::init(config);
  if let Err(e) = crawl(&mut conn, &mut state).and_then(|()| state.create_surfaces(&mut conn)) {
    write_wire_diagnostic("[rustbg]", &e);
    return 1;
  }

  loop {
    match dispatch_once(&mut conn, &mut state) {
      Ok(()) => (),
      Err(ConnectionClosed) => return 0,
      Err(e) => {
        write_wire_diagnostic("[rustbg]", &e);
        return 1;
      }
    }
  }
}
