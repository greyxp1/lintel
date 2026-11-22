use std::cell::RefCell;
use std::collections::HashMap;
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::os::unix::fs::FileExt;
use std::rc::Rc;
use std::time::{Duration, Instant};

use cairo::{Context, Format, ImageSurface};
use memmap2::MmapMut;
use rustix::fs::{MemfdFlags, ftruncate, memfd_create};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
  wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_output, wl_pointer, wl_region,
  wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::ext::background_effect::v1::client::{
  ext_background_effect_manager_v1, ext_background_effect_surface_v1,
};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

use crate::ui::{self, Action, HEIGHT, Layer, Model, POPUP_HEIGHT, POPUP_WIDTH, WIDTH};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const CARET_PERIOD: Duration = Duration::from_millis(500);

struct Buffer {
  handle: wl_buffer::WlBuffer,
  image: ImageSurface,
  busy: bool,
  damage: Damage,
}

#[derive(Clone, Copy, Default)]
enum Damage {
  #[default]
  Clean,
  Region(ui::BarRegion),
  Full,
}

impl Damage {
  fn add(&mut self, region: ui::BarRegion) {
    *self = match *self {
      Self::Clean => Self::Region(region),
      Self::Region(pending) if pending == region => *self,
      _ => Self::Full,
    };
  }
}

impl Drop for Buffer {
  fn drop(&mut self) {
    self.handle.destroy();
  }
}

struct Surface {
  blur: Option<ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1>,
  blur_size: Option<(i32, i32, bool)>,
  backing: Vec<Buffer>,
  surface: wl_surface::WlSurface,
  layer: zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
  requested: (i32, i32),
  configure: Option<(i32, i32)>,
  scale: i32,
  dirty: bool,
  damage: Damage,
  painted: Option<ImageSurface>,
  motion: Option<BarMotion>,
  frame: Option<wl_callback::WlCallback>,
}

struct BarMotion {
  started: Instant,
  from: i32,
  to: i32,
  closing: Option<ImageSurface>,
  wide: bool,
}

impl BarMotion {
  const DURATION: Duration = Duration::from_millis(115);

  fn height(&self) -> i32 {
    let fraction = (self.started.elapsed().as_secs_f64() / Self::DURATION.as_secs_f64()).min(1.0);
    let eased = fraction * fraction * (3.0 - 2.0 * fraction);
    self.from + (f64::from(self.to - self.from) * eased).round() as i32
  }
}

impl Surface {
  fn animate_height(&mut self, target: i32) -> Result<i32> {
    let current = bar_content_height(self.requested.1);
    let previous_target = self.motion.as_ref().map_or(current, |motion| motion.to);
    if previous_target != target {
      let from = self.motion.as_ref().map_or(current, BarMotion::height);
      let mut closing = None;
      if from > target
        && target <= HEIGHT
        && let Some(image) = &self.painted
      {
        let copy = ImageSurface::create(Format::ARgb32, image.width(), image.height())?;
        let cr = Context::new(&copy)?;
        cr.set_source_surface(image, 0.0, 0.0)?;
        cr.paint()?;
        closing = Some(copy);
      }
      self.motion = Some(BarMotion {
        started: Instant::now(),
        from,
        to: target,
        closing,
        wide: self.blur_size.is_some_and(|(_, _, wide)| wide),
      });
      self.dirty = true;
    }
    if let Some(motion) = &self.motion {
      if motion.started.elapsed() < BarMotion::DURATION {
        return Ok(current.max(target).max(motion.from));
      }
      self.motion = None;
      self.dirty = true;
    }
    Ok(target)
  }
}

fn bar_content_height(height: i32) -> i32 {
  if height > ui::REVEAL_HEIGHT {
    height - ui::SHADOW_DEPTH
  } else {
    height
  }
}

fn bar_buffer_height(content: i32) -> i32 {
  if content > ui::REVEAL_HEIGHT {
    content + ui::SHADOW_DEPTH
  } else {
    content
  }
}

impl Drop for Surface {
  fn drop(&mut self) {
    if let Some(blur) = &self.blur {
      blur.destroy();
    }
    self.layer.destroy();
    self.surface.destroy();
  }
}

pub struct Desktop {
  conn: Connection,
  queue: Option<EventQueue<Self>>,
  qh: QueueHandle<Self>,
  compositor: wl_compositor::WlCompositor,
  shm: wl_shm::WlShm,
  shell: zwlr_layer_shell_v1::ZwlrLayerShellV1,
  effects: Option<ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1>,
  bar: Option<Surface>,
  popup: Option<Surface>,
  corners: HashMap<(u32, u8), Surface>,
  outputs: HashMap<u32, wl_output::WlOutput>,
  pointer: Option<wl_pointer::WlPointer>,
  keyboard: Option<wl_keyboard::WlKeyboard>,
  keyboard_state: Option<xkbcommon::xkb::State>,
  keyboard_repeat: Option<(Duration, Duration)>,
  repeated_key: Option<(u32, Instant)>,
  caret_tick: Option<Instant>,
  bar_keyboard_focus: bool,
  bar_on_demand: bool,
  target: Option<Layer>,
  xy: (f64, f64),
  model: Rc<RefCell<Model>>,
  actions: Vec<Action>,
  closed: bool,
}

impl Desktop {
  pub fn connect(model: Rc<RefCell<Model>>) -> Result<Self> {
    let conn = Connection::connect_to_env()?;
    let (globals, queue) = registry_queue_init::<Self>(&conn)?;
    let qh = queue.handle();
    let compositor = globals.bind(&qh, 4..=6, ())?;
    let shm = globals.bind(&qh, 1..=1, ())?;
    let shell = globals.bind(&qh, 4..=5, ())?;
    let effects = globals.bind(&qh, 1..=1, ()).ok();
    let _ = globals.bind::<wl_seat::WlSeat, _, _>(&qh, 5..=5, ());
    let mut desktop = Self {
      conn,
      queue: Some(queue),
      qh,
      compositor,
      shm,
      shell,
      effects,
      bar: None,
      popup: None,
      corners: HashMap::new(),
      outputs: HashMap::new(),
      pointer: None,
      keyboard: None,
      keyboard_state: None,
      keyboard_repeat: None,
      repeated_key: None,
      caret_tick: None,
      bar_keyboard_focus: false,
      bar_on_demand: true,
      target: None,
      xy: (0.0, 0.0),
      model,
      actions: Vec::new(),
      closed: false,
    };
    desktop.bar = Some(desktop.create(Layer::Bar, (WIDTH, ui::REVEAL_HEIGHT), None));
    for global in globals.contents().clone_list() {
      if global.interface == "wl_output" {
        desktop.add_output(globals.registry(), global.name, global.version);
      }
    }
    desktop.flush()?;
    Ok(desktop)
  }

  fn add_output(&mut self, registry: &wl_registry::WlRegistry, name: u32, version: u32) {
    if self.outputs.contains_key(&name) {
      return;
    }
    let output = registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), &self.qh, ());
    for corner in 0..4 {
      let size = (ui::CORNER_SIZE, ui::CORNER_SIZE);
      let mask = self.create(Layer::Corner(name, corner), size, Some(&output));
      self.corners.insert((name, corner), mask);
    }
    self.outputs.insert(name, output);
  }

  fn create(&self, kind: Layer, size: (i32, i32), output: Option<&wl_output::WlOutput>) -> Surface {
    use zwlr_layer_shell_v1::Layer as Level;
    use zwlr_layer_surface_v1::Anchor;
    let surface = self.compositor.create_surface(&self.qh, kind);
    let (namespace, level, anchor) = match kind {
      Layer::Bar => ("top-bar", Level::Top, Anchor::Top),
      Layer::Popup => (
        "desktop-notification",
        Level::Overlay,
        Anchor::Top | Anchor::Right,
      ),
      Layer::Corner(_, corner) => {
        let vertical = if corner & 2 == 0 {
          Anchor::Top
        } else {
          Anchor::Bottom
        };
        let horizontal = if corner & 1 == 0 {
          Anchor::Left
        } else {
          Anchor::Right
        };
        ("desktop-screen-corner", Level::Top, vertical | horizontal)
      }
    };
    let layer =
      self
        .shell
        .get_layer_surface(&surface, output, level, namespace.into(), &self.qh, kind);
    layer.set_anchor(anchor);
    match kind {
      Layer::Bar => {
        layer.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::OnDemand)
      }
      Layer::Popup => layer.set_margin(HEIGHT + 8, 16, 0, 0),
      Layer::Corner(..) => {
        layer.set_exclusive_zone(-1);
        let region = self.compositor.create_region(&self.qh, ());
        surface.set_input_region(Some(&region));
        region.destroy();
      }
    }
    layer.set_size(size.0 as u32, size.1 as u32);
    let blur = if !matches!(kind, Layer::Corner(..)) {
      self
        .effects
        .as_ref()
        .map(|effects| effects.get_background_effect(&surface, &self.qh, ()))
    } else {
      None
    };
    surface.commit();
    Surface {
      blur,
      blur_size: None,
      backing: Vec::new(),
      surface,
      layer,
      requested: size,
      configure: None,
      scale: 1,
      dirty: true,
      damage: Damage::Clean,
      painted: None,
      motion: None,
      frame: None,
    }
  }

  pub fn fd(&self) -> RawFd {
    self.conn.backend().poll_fd().as_raw_fd()
  }

  pub fn dispatch(&mut self) -> Result<Vec<Action>> {
    let mut queue = self.queue.take().expect("Wayland queue");
    queue.dispatch_pending(self)?;
    if let Some(read) = queue.prepare_read() {
      match read.read() {
        Ok(_) => {}
        Err(wayland_client::backend::WaylandError::Io(err))
          if err.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(err) => return Err(err.into()),
      }
      queue.dispatch_pending(self)?;
    }
    self.queue = Some(queue);
    if self.closed {
      return Err("Wayland bar closed".into());
    }
    Ok(std::mem::take(&mut self.actions))
  }

  pub fn flush(&self) -> Result<bool> {
    match self.conn.flush() {
      Ok(()) => Ok(true),
      Err(wayland_client::backend::WaylandError::Io(err))
        if err.kind() == std::io::ErrorKind::WouldBlock =>
      {
        Ok(false)
      }
      Err(err) => Err(err.into()),
    }
  }

  pub fn redraw(&mut self) -> Result<bool> {
    if let Some(bar) = &mut self.bar {
      bar.dirty = true;
    }
    if let Some(popup) = &mut self.popup {
      popup.dirty = true;
    }
    self.draw_pending()
  }

  pub fn repaint_bar(&mut self, region: ui::BarRegion) {
    if let Some(bar) = &mut self.bar {
      bar.damage.add(region);
    }
  }

  pub fn draw_pending(&mut self) -> Result<bool> {
    self.update_search_focus();
    let model = self.model.borrow();
    let popup_height = model.popup_rows().last().map(|y| y + POPUP_HEIGHT);
    if let Some(height) = popup_height {
      let wanted = (POPUP_WIDTH, height);
      if let Some(popup) = &mut self.popup {
        if popup.requested != wanted {
          popup.layer.set_size(wanted.0 as u32, wanted.1 as u32);
          popup.surface.commit();
          popup.requested = wanted;
        }
      } else {
        self.popup = Some(self.create(Layer::Popup, wanted, None));
      }
    } else {
      self.popup = None;
    }
    if let Some(bar) = &mut self.bar {
      let on_demand = model.panel != ui::Panel::None || !self.bar_keyboard_focus;
      if self.bar_on_demand != on_demand {
        self.bar_on_demand = on_demand;
        use zwlr_layer_surface_v1::KeyboardInteractivity;
        bar.layer.set_keyboard_interactivity(if on_demand {
          KeyboardInteractivity::OnDemand
        } else {
          KeyboardInteractivity::None
        });
        bar.surface.commit();
      }
      let wanted = (
        WIDTH,
        bar_buffer_height(bar.animate_height(model.bar_height())?),
      );
      if bar.requested != wanted {
        bar.layer.set_size(wanted.0 as u32, wanted.1 as u32);
        bar.surface.commit();
        bar.requested = wanted;
      }
      Self::draw(
        &self.shm,
        &self.compositor,
        &self.qh,
        bar,
        &model,
        Layer::Bar,
      )?;
    }
    if let Some(popup) = &mut self.popup {
      Self::draw(
        &self.shm,
        &self.compositor,
        &self.qh,
        popup,
        &model,
        Layer::Popup,
      )?;
    }
    for (&(name, corner), surface) in &mut self.corners {
      Self::draw(
        &self.shm,
        &self.compositor,
        &self.qh,
        surface,
        &model,
        Layer::Corner(name, corner),
      )?;
    }
    self.flush()
  }

  fn update_search_focus(&mut self) {
    let mut model = self.model.borrow_mut();
    if self.bar_keyboard_focus && model.search_keyboard() {
      if self.caret_tick.is_none() {
        self.caret_tick = Some(Instant::now() + CARET_PERIOD);
        model.search_cursor_visible = true;
        if let Some(bar) = &mut self.bar {
          bar.dirty = true;
        }
      }
    } else {
      self.repeated_key = None;
      if self.caret_tick.take().is_some() {
        model.search_cursor_visible = false;
        if let Some(bar) = &mut self.bar {
          bar.dirty = true;
        }
      }
    }
  }

  pub fn search_deadline(&self) -> Option<Instant> {
    self
      .caret_tick
      .into_iter()
      .chain(self.repeated_key.map(|(_, deadline)| deadline))
      .chain(self.model.borrow().search_pending)
      .min()
  }

  pub fn tick_search(&mut self) {
    self.update_search_focus();
    let now = Instant::now();
    if let Some((key, deadline)) = self.repeated_key
      && deadline <= now
    {
      self.repeated_key = self
        .keyboard_repeat
        .map(|(_, interval)| (key, now + interval));
      self.edit_search_key(key);
    }
    if self.caret_tick.is_some_and(|deadline| deadline <= now) {
      self.caret_tick = Some(now + CARET_PERIOD);
      let mut model = self.model.borrow_mut();
      model.search_cursor_visible = !model.search_cursor_visible;
      drop(model);
      self.repaint_bar(ui::BarRegion::Search);
    }
  }

  fn edit_search_key(&mut self, key: u32) -> bool {
    use xkbcommon::xkb::{Keycode, keysyms};
    let Some(keyboard) = &self.keyboard_state else {
      return false;
    };
    let key = Keycode::new(key + 8);
    let mut model = self.model.borrow_mut();
    if !self.bar_keyboard_focus || !model.search_keyboard() {
      return false;
    }
    let query = &model.search_query;
    let cursor = model.search_cursor;
    let before = query[..cursor]
      .char_indices()
      .next_back()
      .map_or(cursor, |(index, _)| index);
    let after = cursor + query[cursor..].chars().next().map_or(0, char::len_utf8);
    let previous_len = query.len();
    match keyboard.key_get_one_sym(key).raw() {
      keysyms::KEY_Left => model.search_cursor = before,
      keysyms::KEY_Right => model.search_cursor = after,
      keysyms::KEY_Home => model.search_cursor = 0,
      keysyms::KEY_End => model.search_cursor = previous_len,
      keysyms::KEY_BackSpace => {
        model.search_query.replace_range(before..cursor, "");
        model.search_cursor = before;
      }
      keysyms::KEY_Delete => {
        model.search_query.replace_range(cursor..after, "");
      }
      _ => {
        let input = keyboard.key_get_utf8(key);
        if input.is_empty()
          || previous_len + input.len() > 256
          || input.chars().any(char::is_control)
        {
          return false;
        }
        model.search_query.insert_str(cursor, &input);
        model.search_cursor += input.len();
      }
    }
    let edited = model.search_query.len() != previous_len;
    if edited {
      model.search_edited();
    }
    model.search_cursor_visible = true;
    self.caret_tick = Some(Instant::now() + CARET_PERIOD);
    drop(model);
    if edited {
      if let Some(bar) = &mut self.bar {
        bar.dirty = true;
      }
    } else {
      self.repaint_bar(ui::BarRegion::Search);
    }
    true
  }

  pub fn animate_bar(&mut self) -> bool {
    let Some(bar) = &mut self.bar else {
      return false;
    };
    let model = self.model.borrow();
    if bar.motion.is_some() || model.panel_hover_animating() {
      bar.dirty = true;
      return true;
    }
    if model.header_hover_animating() {
      bar.damage.add(ui::BarRegion::Header);
      return true;
    }
    false
  }

  fn draw(
    shm: &wl_shm::WlShm,
    compositor: &wl_compositor::WlCompositor,
    qh: &QueueHandle<Self>,
    surface: &mut Surface,
    model: &Model,
    kind: Layer,
  ) -> Result<()> {
    let Some((width, height)) = surface.configure else {
      return Ok(());
    };
    if (!surface.dirty && matches!(surface.damage, Damage::Clean))
      || surface.requested != (width, height)
    {
      return Ok(());
    }
    let scale = surface.scale;
    let (pixel_w, pixel_h) = (width * scale, height * scale);
    surface
      .backing
      .retain(|b| b.image.width() == pixel_w && b.image.height() == pixel_h);
    // A released buffer can be several frames behind the one on screen.
    for buffer in &mut surface.backing {
      if surface.dirty || surface.motion.is_some() {
        buffer.damage = Damage::Full;
      } else {
        match surface.damage {
          Damage::Region(region) => buffer.damage.add(region),
          Damage::Full => buffer.damage = Damage::Full,
          Damage::Clean => {}
        }
      }
    }
    let free = surface.backing.iter().position(|buffer| !buffer.busy);
    let index = match free {
      Some(index) => index,
      None if surface.backing.len() < 2 => {
        let stride = Format::ARgb32.stride_for_width(pixel_w as u32)?;
        let bytes = stride * pixel_h;
        let file = memfd_create(c"lintel", MemfdFlags::CLOEXEC)?;
        ftruncate(&file, bytes as u64)?;
        // Each mapping is written only after the compositor releases its buffer.
        let pixels = unsafe { MmapMut::map_mut(&file)? };
        let pool = shm.create_pool(file.as_fd(), bytes, qh, ());
        let handle = pool.create_buffer(
          0,
          pixel_w,
          pixel_h,
          stride,
          wl_shm::Format::Argb8888,
          qh,
          kind,
        );
        pool.destroy();
        let image =
          ImageSurface::create_for_data(pixels, Format::ARgb32, pixel_w, pixel_h, stride)?;
        surface.backing.push(Buffer {
          handle,
          image,
          busy: false,
          damage: Damage::Full,
        });
        surface.backing.len() - 1
      }
      None => return Ok(()),
    };
    let buffer = &mut surface.backing[index];
    let cr = Context::new(&buffer.image)?;
    cr.scale(f64::from(scale), f64::from(scale));
    if let Damage::Region(region) = buffer.damage {
      let (x, y, w, h) = region.bounds();
      cr.rectangle(f64::from(x), f64::from(y), f64::from(w), f64::from(h));
      cr.clip();
      region.paint(&cr, model)?;
      drop(cr);
      buffer.image.flush();
      surface.surface.attach(Some(&buffer.handle), 0, 0);
      surface
        .surface
        .damage_buffer(x * scale, y * scale, w * scale, h * scale);
      surface.surface.commit();
      buffer.busy = true;
      buffer.damage = Damage::Clean;
      surface.damage = Damage::Clean;
      surface.painted = Some(buffer.image.clone());
      return Ok(());
    }
    let content_height = if kind == Layer::Bar {
      bar_content_height(height)
    } else {
      height
    };
    let visible_height = surface
      .motion
      .as_ref()
      .map_or(content_height, BarMotion::height)
      .min(content_height);
    if let Layer::Corner(_, corner) = kind {
      let size = f64::from(ui::CORNER_SIZE);
      cr.set_operator(cairo::Operator::Source);
      cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
      cr.paint()?;
      cr.set_fill_rule(cairo::FillRule::EvenOdd);
      cr.rectangle(0.0, 0.0, size, size);
      let x = if corner & 1 == 0 { size } else { 0.0 };
      let y = if corner & 2 == 0 { size } else { 0.0 };
      cr.new_sub_path();
      cr.arc(x, y, size, 0.0, std::f64::consts::TAU);
      cr.set_source_rgb(0.0, 0.0, 0.0);
      cr.fill()?;
    } else if kind == Layer::Bar {
      cr.set_operator(cairo::Operator::Clear);
      cr.paint()?;
      cr.set_operator(cairo::Operator::Over);
      cr.rectangle(
        0.0,
        0.0,
        f64::from(width),
        f64::from(bar_buffer_height(visible_height)),
      );
      cr.clip();
      if let Some(image) = surface
        .motion
        .as_ref()
        .and_then(|motion| motion.closing.as_ref())
      {
        cr.save()?;
        cr.scale(1.0 / f64::from(scale), 1.0 / f64::from(scale));
        cr.set_source_surface(image, 0.0, 0.0)?;
        cr.paint()?;
        cr.restore()?;
      } else {
        ui::paint(&cr, model, kind)?;
      }
      surface.painted = Some(buffer.image.clone());
    } else {
      ui::paint(&cr, model, kind)?;
    }
    if kind == Layer::Bar {
      let region = compositor.create_region(qh, ());
      let left = (WIDTH - ui::PANEL_WIDTH) / 2;
      region.add(left, 0, ui::PANEL_WIDTH, visible_height.min(HEIGHT));
      if visible_height > HEIGHT {
        let left = if model.wide_music() { ui::WING } else { left };
        region.add(left, HEIGHT, WIDTH - 2 * left, visible_height - HEIGHT);
      }
      surface.surface.set_input_region(Some(&region));
      region.destroy();
    }
    let outline_height = surface
      .motion
      .as_ref()
      .filter(|motion| motion.closing.is_some())
      .map_or(model.bar_height(), |motion| motion.from.max(HEIGHT));
    let wide = surface
      .motion
      .as_ref()
      .filter(|motion| motion.closing.is_some())
      .map_or(model.wide_music(), |motion| motion.wide);
    let blur_size = if kind == Layer::Bar {
      (visible_height, outline_height, wide)
    } else {
      (height, height, false)
    };
    if !matches!(kind, Layer::Corner(..)) && surface.blur_size != Some(blur_size) {
      if surface.blur.is_some() || kind == Layer::Popup {
        let region = compositor.create_region(qh, ());
        cr.reset_clip();
        if kind == Layer::Popup {
          for y in model.popup_rows() {
            cr.new_path();
            ui::round(&cr, 0, y, width, POPUP_HEIGHT, ui::POPUP_RADIUS);
            add_blur_rows(
              &cr,
              &region,
              width,
              y,
              POPUP_HEIGHT,
              ui::POPUP_RADIUS as i32,
            )?;
          }
          surface.surface.set_input_region(Some(&region));
        } else if visible_height > ui::REVEAL_HEIGHT {
          ui::bar_outline(&cr, outline_height, wide);
          add_blur_rows(
            &cr,
            &region,
            width,
            0,
            visible_height,
            ui::HEADER_X + ui::WING + 17,
          )?;
        }
        if let Some(blur) = &surface.blur {
          blur.set_blur_region(Some(&region));
        }
        region.destroy();
      }
      surface.blur_size = Some(blur_size);
    }
    drop(cr);
    buffer.image.flush();
    surface.surface.set_buffer_scale(scale);
    surface.surface.attach(Some(&buffer.handle), 0, 0);
    surface.surface.damage_buffer(0, 0, pixel_w, pixel_h);
    if kind == Layer::Popup && model.timed_popups() && surface.frame.is_none() {
      surface.frame = Some(surface.surface.frame(qh, kind));
    }
    surface.surface.commit();
    buffer.busy = true;
    surface.dirty = false;
    surface.damage = Damage::Clean;
    buffer.damage = Damage::Clean;
    Ok(())
  }

  fn surface(&mut self, kind: Layer) -> Option<&mut Surface> {
    match kind {
      Layer::Bar => self.bar.as_mut(),
      Layer::Popup => self.popup.as_mut(),
      Layer::Corner(name, corner) => self.corners.get_mut(&(name, corner)),
    }
  }
}

fn add_blur_rows(
  cr: &Context,
  region: &wl_region::WlRegion,
  width: i32,
  top: i32,
  height: i32,
  max_inset: i32,
) -> Result<()> {
  // Both outlines are horizontally symmetric. Merge consecutive rows with the same inset.
  let mut start = top;
  let mut previous = 0;
  for y in top..top + height {
    let mut inset = 0;
    let mut end = max_inset;
    while inset < end {
      let middle = (inset + end) / 2;
      if cr.in_fill(f64::from(middle) + 0.5, f64::from(y) + 0.5)? {
        end = middle;
      } else {
        inset = middle + 1;
      }
    }
    if inset != previous {
      if y > start {
        region.add(previous, start, width - 2 * previous, y - start);
      }
      start = y;
      previous = inset;
    }
  }
  region.add(previous, start, width - 2 * previous, top + height - start);
  Ok(())
}

delegate_noop!(Desktop: ignore wl_output::WlOutput);
delegate_noop!(Desktop: ignore wl_compositor::WlCompositor);
delegate_noop!(Desktop: ignore wl_shm::WlShm);
delegate_noop!(Desktop: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Desktop: ignore wl_region::WlRegion);
delegate_noop!(Desktop: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);
delegate_noop!(Desktop: ignore ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1);
delegate_noop!(Desktop: ignore ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1);

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Desktop {
  fn event(
    desktop: &mut Self,
    registry: &wl_registry::WlRegistry,
    event: wl_registry::Event,
    _: &GlobalListContents,
    _: &Connection,
    _: &QueueHandle<Self>,
  ) {
    match event {
      wl_registry::Event::Global {
        name,
        interface,
        version,
      } if interface == "wl_output" => {
        desktop.add_output(registry, name, version);
      }
      wl_registry::Event::GlobalRemove { name } => {
        if let Some(output) = desktop.outputs.remove(&name) {
          desktop.corners.retain(|(id, _), _| *id != name);
          if output.version() >= 3 {
            output.release();
          }
        }
      }
      _ => {}
    }
  }
}

impl Dispatch<wl_surface::WlSurface, Layer> for Desktop {
  fn event(
    state: &mut Self,
    _: &wl_surface::WlSurface,
    event: wl_surface::Event,
    kind: &Layer,
    _: &Connection,
    _: &QueueHandle<Self>,
  ) {
    if let wl_surface::Event::PreferredBufferScale { factor } = event
      && let Some(surface) = state.surface(*kind)
    {
      surface.scale = factor.clamp(1, 4);
      surface.dirty = true;
    }
  }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, Layer> for Desktop {
  fn event(
    state: &mut Self,
    proxy: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
    event: zwlr_layer_surface_v1::Event,
    kind: &Layer,
    _: &Connection,
    _: &QueueHandle<Self>,
  ) {
    match event {
      zwlr_layer_surface_v1::Event::Configure {
        serial,
        width,
        height,
      } => {
        proxy.ack_configure(serial);
        if let Some(surface) = state.surface(*kind) {
          let (w, h) = surface.requested;
          surface.configure = Some((
            if width == 0 { w } else { width as i32 },
            if height == 0 { h } else { height as i32 },
          ));
          surface.dirty = true;
        }
      }
      zwlr_layer_surface_v1::Event::Closed => match *kind {
        Layer::Bar => state.closed = true,
        Layer::Popup => state.popup = None,
        Layer::Corner(name, corner) => {
          state.corners.remove(&(name, corner));
        }
      },
      _ => {}
    }
  }
}

impl Dispatch<wl_callback::WlCallback, Layer> for Desktop {
  fn event(
    state: &mut Self,
    callback: &wl_callback::WlCallback,
    _: wl_callback::Event,
    kind: &Layer,
    _: &Connection,
    _: &QueueHandle<Self>,
  ) {
    let animate = state.model.borrow().timed_popups();
    if let Some(surface) = state.surface(*kind)
      && surface.frame.as_ref() == Some(callback)
    {
      surface.frame = None;
      surface.dirty |= animate;
    }
  }
}

impl Dispatch<wl_buffer::WlBuffer, Layer> for Desktop {
  fn event(
    state: &mut Self,
    proxy: &wl_buffer::WlBuffer,
    _: wl_buffer::Event,
    kind: &Layer,
    _: &Connection,
    _: &QueueHandle<Self>,
  ) {
    if let Some(surface) = state.surface(*kind)
      && let Some(buffer) = surface
        .backing
        .iter_mut()
        .find(|buffer| &buffer.handle == proxy)
    {
      buffer.busy = false;
    }
  }
}

impl Dispatch<wl_seat::WlSeat, ()> for Desktop {
  fn event(
    state: &mut Self,
    seat: &wl_seat::WlSeat,
    event: wl_seat::Event,
    _: &(),
    _: &Connection,
    qh: &QueueHandle<Self>,
  ) {
    if let wl_seat::Event::Capabilities {
      capabilities: WEnum::Value(caps),
    } = event
    {
      if caps.contains(wl_seat::Capability::Keyboard) {
        if state.keyboard.is_none() {
          state.keyboard = Some(seat.get_keyboard(qh, ()));
        }
      } else if let Some(keyboard) = state.keyboard.take() {
        keyboard.release();
        state.keyboard_state = None;
        state.bar_keyboard_focus = false;
      }
      if caps.contains(wl_seat::Capability::Pointer) {
        if state.pointer.is_none() {
          state.pointer = Some(seat.get_pointer(qh, ()));
        }
      } else if let Some(pointer) = state.pointer.take() {
        pointer.release();
      }
    }
  }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Desktop {
  fn event(
    state: &mut Self,
    _: &wl_keyboard::WlKeyboard,
    event: wl_keyboard::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Self>,
  ) {
    use wl_keyboard::Event;
    use xkbcommon::xkb;
    match event {
      Event::Keymap {
        format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1),
        fd,
        size,
      } => {
        let mut bytes = vec![0; size as usize];
        if std::fs::File::from(fd).read_exact_at(&mut bytes, 0).is_ok()
          && let Ok(mut keymap) = String::from_utf8(bytes)
        {
          keymap.truncate(keymap.trim_end_matches('\0').len());
          let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
          if let Some(map) = xkb::Keymap::new_from_string(
            &context,
            keymap,
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
          ) {
            state.keyboard_state = Some(xkb::State::new(&map));
          }
        }
      }
      Event::Enter { surface, .. } if surface.data::<Layer>() == Some(&Layer::Bar) => {
        state.bar_keyboard_focus = true;
      }
      Event::Leave { surface, .. } if surface.data::<Layer>() == Some(&Layer::Bar) => {
        state.bar_keyboard_focus = false;
        state.repeated_key = None;
        state.actions.push(Action::BarFocusLost);
      }
      Event::RepeatInfo { rate, delay } => {
        state.keyboard_repeat = (rate > 0).then(|| {
          (
            Duration::from_millis(delay.max(0) as u64),
            Duration::from_millis((1000 / rate as u64).max(1)),
          )
        });
        state.repeated_key = None;
      }
      Event::Modifiers {
        mods_depressed,
        mods_latched,
        mods_locked,
        group,
        ..
      } => {
        if let Some(keyboard) = &mut state.keyboard_state {
          keyboard.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
        }
      }
      Event::Key {
        key,
        state: WEnum::Value(wl_keyboard::KeyState::Pressed),
        ..
      } if state.bar_keyboard_focus => {
        let Some(keyboard) = &state.keyboard_state else {
          return;
        };
        let code = xkb::Keycode::new(key + 8);
        let symbol = keyboard.key_get_one_sym(code);
        let repeatable = keyboard.get_keymap().key_repeats(code);
        let ctrl = keyboard.mod_name_is_active(xkb::MOD_NAME_CTRL, xkb::STATE_MODS_EFFECTIVE);
        let raw = symbol.raw();
        if raw == xkb::keysyms::KEY_Escape {
          state.actions.push(Action::DismissBar);
        } else if ctrl && (xkb::keysyms::KEY_1..=xkb::keysyms::KEY_4).contains(&raw) {
          if let Some(action) = state
            .model
            .borrow_mut()
            .music_tab_shortcut((raw - xkb::keysyms::KEY_1) as usize)
          {
            state.actions.push(action);
            if let Some(bar) = &mut state.bar {
              bar.dirty = true;
            }
          }
        } else if matches!(raw, xkb::keysyms::KEY_Up | xkb::keysyms::KEY_Down) {
          if state
            .model
            .borrow_mut()
            .move_search_focus(raw == xkb::keysyms::KEY_Down)
          {
            state.actions.push(Action::SmallCovers);
            if let Some(bar) = &mut state.bar {
              bar.dirty = true;
            }
          }
        } else if matches!(raw, xkb::keysyms::KEY_Return | xkb::keysyms::KEY_KP_Enter) {
          state.repeated_key = None;
          let mut model = state.model.borrow_mut();
          if let Some(action) = model.activate_search_focus(ctrl) {
            state.actions.push(action);
            if let Some(bar) = &mut state.bar {
              bar.dirty = true;
            }
          } else if model.search_keyboard()
            && !model.search_has_focus_row()
            && !model.search_query.trim().is_empty()
          {
            state
              .actions
              .push(Action::Search(model.search_query.trim().to_owned()));
          }
        } else if state.edit_search_key(key) {
          state.repeated_key = state
            .keyboard_repeat
            .filter(|_| repeatable)
            .map(|(delay, _)| (key, Instant::now() + delay));
        }
      }
      Event::Key {
        key,
        state: WEnum::Value(wl_keyboard::KeyState::Released),
        ..
      } => {
        if state.repeated_key.is_some_and(|(held, _)| held == key) {
          state.repeated_key = None;
        }
      }
      _ => {}
    }
  }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Desktop {
  fn event(
    state: &mut Self,
    _: &wl_pointer::WlPointer,
    event: wl_pointer::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Self>,
  ) {
    use wl_pointer::Event;
    match event {
      Event::Enter {
        surface,
        surface_x,
        surface_y,
        ..
      } => {
        state.target = surface.data::<Layer>().copied();
        state.xy = (surface_x, surface_y);
        if state.target == Some(Layer::Popup)
          && state.model.borrow_mut().hover_popup(Some(state.xy))
        {
          state.actions.push(Action::PopupHover);
        }
        if state.target == Some(Layer::Bar) {
          let mut model = state.model.borrow_mut();
          model.pointer_generation = model.pointer_generation.wrapping_add(1);
          model.bar_visible = true;
          model.hover_bar(Some(state.xy));
          if let Some(bar) = &mut state.bar {
            bar.dirty = true;
          }
        }
      }
      Event::Leave { .. } => {
        if state.target == Some(Layer::Bar) {
          let mut model = state.model.borrow_mut();
          if model.hover_bar(None)
            && let Some(bar) = &mut state.bar
          {
            bar.dirty = true;
          }
          model.pointer_generation = model.pointer_generation.wrapping_add(1);
          state
            .actions
            .push(Action::HideBar(model.pointer_generation));
        }
        if state.target == Some(Layer::Popup) && state.model.borrow_mut().hover_popup(None) {
          state.actions.push(Action::PopupHover);
        }
        state.target = None;
      }
      Event::Motion {
        surface_x,
        surface_y,
        ..
      } => {
        state.xy = (surface_x, surface_y);
        if state.target == Some(Layer::Popup)
          && state.model.borrow_mut().hover_popup(Some(state.xy))
        {
          state.actions.push(Action::PopupHover);
        }
        if state.target == Some(Layer::Bar) {
          let mut model = state.model.borrow_mut();
          let mut changed = model.hover_bar(Some(state.xy));
          changed |= model.drag_music(surface_x as i32, surface_y as i32);
          if let Some((slider, value)) = &mut model.slider_preview {
            let next = slider.position(surface_x as i32, surface_y as i32);
            if *value != next {
              changed = true;
              *value = next;
              state.actions.push(Action::Volume(*slider, Some(next)));
            }
          }
          if changed && let Some(bar) = &mut state.bar {
            bar.dirty = true;
          }
        }
      }
      Event::Button {
        button,
        state: WEnum::Value(wl_pointer::ButtonState::Pressed),
        ..
      } if matches!(button, 0x110 | 0x111) => {
        let Some(target) = state.target else { return };
        let (x, y) = state.xy;
        let model = &mut *state.model.borrow_mut();
        match model.hit(target, x as i32, y as i32, button == 0x111) {
          Some(Action::Volume(slider, Some(value))) => {
            model.slider_preview = Some((slider, value));
            state.actions.push(Action::Volume(slider, Some(value)));
          }
          Some(action) => state.actions.push(action),
          None => {}
        }
        if let Some(bar) = &mut state.bar {
          bar.dirty = true;
        }
      }
      Event::Button {
        button: 0x110,
        state: WEnum::Value(wl_pointer::ButtonState::Released),
        ..
      } => {
        let mut model = state.model.borrow_mut();
        let slider = model.slider_preview.take().is_some();
        if let Some(action) = model.finish_music_drag() {
          state.actions.push(action);
        }
        if (slider || state.target == Some(Layer::Bar))
          && let Some(bar) = &mut state.bar
        {
          bar.dirty = true;
        }
      }
      Event::Axis {
        axis: WEnum::Value(wl_pointer::Axis::VerticalScroll),
        value,
        ..
      } if value != 0.0 && state.target == Some(Layer::Bar) => {
        let mut model = state.model.borrow_mut();
        model.scroll(value, state.xy.0 as i32, state.xy.1 as i32);
        if model.panel == ui::Panel::Music {
          state.actions.push(Action::SmallCovers);
        }
        if let Some(bar) = &mut state.bar {
          bar.dirty = true;
        }
      }
      _ => {}
    }
  }
}
