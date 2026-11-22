use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::rc::Rc;
use std::time::Duration;

use gio::glib::{self, Variant, variant::ToVariant};
use gio::{Cancellable, DBusCallFlags, DBusConnection, DBusNodeInfo, DBusSignalFlags};

use crate::media::Art;
use crate::ui::{Address, Countdown, Model, Notice, TrayItem, app_icon, image};

const NOTIFY: &str = "org.freedesktop.Notifications";
const NOTIFY_PATH: &str = "/org/freedesktop/Notifications";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM: &str = "org.kde.StatusNotifierItem";
const BUS: &str = "org.freedesktop.DBus";
const BUS_PATH: &str = "/org/freedesktop/DBus";
const PROPS: &str = "org.freedesktop.DBus.Properties";
const SHELL: &str = "org.lintel.Shell";
const SHELL_PATH: &str = "/org/lintel/Shell";
const HISTORY_LIMIT: usize = 100;
const TRAY_LIMIT: usize = 32;

fn notification_image(hints: &HashMap<String, Variant>, icon: &str) -> Option<cairo::ImageSurface> {
  for key in ["image-data", "image_data", "icon_data"] {
    if let Some(raw) = hints.get(key).and_then(notification_pixels) {
      return Some(raw);
    }
  }
  for candidate in [
    hints
      .get("image-path")
      .or_else(|| hints.get("image_path"))
      .and_then(Variant::str),
    Some(icon),
    hints.get("desktop-entry").and_then(Variant::str),
  ]
  .into_iter()
  .flatten()
  {
    let local = if candidate.starts_with("file://") {
      gio::glib::filename_from_uri(candidate)
        .ok()
        .filter(|(_, host)| host.as_deref().is_none_or(|host| host == "localhost"))
        .map(|(path, _)| path)
    } else if candidate.starts_with('/') {
      Some(std::path::PathBuf::from(candidate))
    } else {
      if let Some(icon) = app_icon(candidate) {
        return Some(icon);
      }
      continue;
    };
    if let Some(path) = local
      && let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
      && file.metadata().is_ok_and(|metadata| metadata.is_file())
    {
      let mut bytes = Vec::new();
      const LIMIT: u64 = 2 << 20;
      if file.take(LIMIT + 1).read_to_end(&mut bytes).is_ok()
        && bytes.len() as u64 <= LIMIT
        && let Some(icon) = crate::media::decode_art(bytes, 48).and_then(image)
      {
        return Some(icon);
      }
    }
  }
  None
}

fn notification_pixels(raw: &Variant) -> Option<cairo::ImageSurface> {
  if raw.type_().as_str() != "(iiibiiay)" {
    return None;
  }
  let width = u32::try_from(raw.child_get::<i32>(0)).ok()?;
  let height = u32::try_from(raw.child_get::<i32>(1)).ok()?;
  let stride = usize::try_from(raw.child_get::<i32>(2)).ok()?;
  let alpha = raw.child_get::<bool>(3);
  let channels = if alpha { 4 } else { 3 };
  if !(1..=512).contains(&width)
    || !(1..=512).contains(&height)
    || raw.child_get::<i32>(4) != 8
    || raw.child_get::<i32>(5) != channels as i32
  {
    return None;
  }
  let row_bytes = width as usize * channels;
  let data = raw.child_value(6);
  let data = data.fixed_array::<u8>().ok()?;
  let size = stride
    .checked_mul(height as usize - 1)?
    .checked_add(row_bytes)?;
  if stride < row_bytes || size > data.len() {
    return None;
  }
  let pixels = ::image::RgbaImage::from_fn(width, height, |x, y| {
    let start = y as usize * stride + x as usize * channels;
    let px = &data[start..start + channels];
    ::image::Rgba([px[0], px[1], px[2], if alpha { px[3] } else { 255 }])
  });
  image(crate::media::thumbnail_art(pixels.into(), 48))
}

const NOTIFY_XML: &str = r#"
<node><interface name="org.freedesktop.Notifications">
  <method name="GetCapabilities"><arg direction="out" type="as"/></method>
  <method name="Notify">
    <arg direction="in" type="s"/><arg direction="in" type="u"/>
    <arg direction="in" type="s"/><arg direction="in" type="s"/>
    <arg direction="in" type="s"/><arg direction="in" type="as"/>
    <arg direction="in" type="a{sv}"/><arg direction="in" type="i"/>
    <arg direction="out" type="u"/>
  </method>
  <method name="CloseNotification"><arg direction="in" type="u"/></method>
  <method name="GetServerInformation">
    <arg direction="out" type="s"/><arg direction="out" type="s"/>
    <arg direction="out" type="s"/><arg direction="out" type="s"/>
  </method>
  <signal name="NotificationClosed"><arg type="u"/><arg type="u"/></signal>
  <signal name="ActionInvoked"><arg type="u"/><arg type="s"/></signal>
</interface></node>
"#;

const TRAY_XML: &str = r#"
<node><interface name="org.kde.StatusNotifierWatcher">
  <method name="RegisterStatusNotifierItem"><arg direction="in" type="s"/></method>
  <method name="RegisterStatusNotifierHost"><arg direction="in" type="s"/></method>
  <property name="RegisteredStatusNotifierItems" type="as" access="read"/>
  <property name="IsStatusNotifierHostRegistered" type="b" access="read"/>
  <property name="ProtocolVersion" type="i" access="read"/>
  <signal name="StatusNotifierItemRegistered"><arg type="s"/></signal>
  <signal name="StatusNotifierItemUnregistered"><arg type="s"/></signal>
  <signal name="StatusNotifierHostRegistered"/>
  <signal name="StatusNotifierHostUnregistered"/>
</interface></node>
"#;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub struct Services {
  bus: DBusConnection,
  model: Rc<RefCell<Model>>,
  refresh: Rc<dyn Fn()>,
  next_id: Cell<u32>,
  expiry: RefCell<HashMap<u32, glib::SourceId>>,
  items: RefCell<Vec<RegisteredItem>>,
  subscriptions: RefCell<Vec<gio::SignalSubscription>>,
}

struct RegisteredItem {
  address: Address,
  owner: String,
}

fn claim(bus: &DBusConnection, name: &str) -> Result<()> {
  let reply = bus.call_sync(
    Some(BUS),
    BUS_PATH,
    BUS,
    "RequestName",
    Some(&(name, 4u32).to_variant()),
    None,
    DBusCallFlags::NONE,
    1500,
    None::<&Cancellable>,
  )?;
  if reply.get::<(u32,)>() != Some((1,)) {
    return Err(format!("{name} is already owned by another desktop service").into());
  }
  Ok(())
}

pub fn toggle_audio() -> Result<()> {
  shell_call("ToggleAudio", None)
}

pub fn step_volume(step: i32) -> Result<()> {
  shell_call("StepVolume", Some(&(step,).to_variant()))
}

fn shell_call(method: &str, args: Option<&Variant>) -> Result<()> {
  let bus = gio::bus_get_sync(gio::BusType::Session, None::<&Cancellable>)?;
  bus.call_sync(
    Some(SHELL),
    SHELL_PATH,
    SHELL,
    method,
    args,
    None,
    DBusCallFlags::NO_AUTO_START,
    1500,
    None::<&Cancellable>,
  )?;
  Ok(())
}

impl Services {
  pub fn connect(model: Rc<RefCell<Model>>, refresh: Rc<dyn Fn()>) -> Result<Rc<Self>> {
    let bus = gio::bus_get_sync(gio::BusType::Session, None::<&Cancellable>)?;
    claim(&bus, NOTIFY)?;
    claim(&bus, WATCHER)?;
    let services = Rc::new(Self {
      bus,
      model,
      refresh,
      next_id: Cell::new(0),
      expiry: RefCell::default(),
      items: RefCell::default(),
      subscriptions: RefCell::default(),
    });
    services.notifications()?;
    services.tray()?;
    Ok(services)
  }

  pub fn bind_audio_panel(
    &self,
    toggle: impl Fn() + 'static,
    step: impl Fn(i32) + 'static,
  ) -> Result<()> {
    claim(&self.bus, SHELL)?;
    let interface = DBusNodeInfo::for_xml(
      "<node><interface name='org.lintel.Shell'><method name='ToggleAudio'/>+      <method name='StepVolume'><arg type='i' direction='in'/></method></interface></node>",
    )?
    .lookup_interface(SHELL)
    .expect("shell interface");
    self
      .bus
      .register_object(SHELL_PATH, &interface)
      .method_call(move |_, _, _, _, method, args, invocation| {
        match method {
          "ToggleAudio" => toggle(),
          "StepVolume" => {
            let amount = args.child_get::<i32>(0);
            if !matches!(amount, -5 | 5) {
              invocation.return_dbus_error(
                "org.freedesktop.DBus.Error.InvalidArgs",
                "Expected a five percent step",
              );
              return;
            }
            step(amount);
          }
          _ => {
            invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method);
            return;
          }
        }
        invocation.return_value(None);
      })
      .build()?;
    Ok(())
  }

  fn notifications(self: &Rc<Self>) -> Result<()> {
    let interface = DBusNodeInfo::for_xml(NOTIFY_XML)?
      .lookup_interface(NOTIFY)
      .expect("notification interface");
    let service = self.clone();
    self
      .bus
      .register_object(NOTIFY_PATH, &interface)
      .method_call(move |_, _, _, _, method, args, invocation| {
        let result = match method {
          "GetCapabilities" => (vec!["actions", "body", "icon-static"],).to_variant(),
          "GetServerInformation" => {
            ("Lintel", "Lintel", env!("CARGO_PKG_VERSION"), "1.2").to_variant()
          }
          "CloseNotification" => {
            service.close(args.child_get(0), 3);
            ().to_variant()
          }
          "Notify" => {
            let id = service.notify(&args);
            (id,).to_variant()
          }
          _ => {
            invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method);
            return;
          }
        };
        invocation.return_value(Some(&result));
      })
      .build()?;
    Ok(())
  }

  fn notify(self: &Rc<Self>, args: &Variant) -> u32 {
    let requested = args.child_get::<u32>(1);
    let mut model = self.model.borrow_mut();
    let id = if requested != 0
      && model
        .notices
        .iter()
        .any(|notice| notice.id == requested && notice.live)
    {
      requested
    } else {
      let id = self.next_id.get().wrapping_add(1).max(1);
      self.next_id.set(id);
      id
    };
    let mut evicted = None;
    if let Some(index) = model.notices.iter().position(|notice| notice.id == id) {
      model.notices.remove(index);
    } else if model.notices.len() >= HISTORY_LIMIT {
      evicted = model.notices.pop_back();
    }
    let actions = args.child_get::<Vec<String>>(5);
    let mut actions = actions.into_iter();
    let actions = std::iter::from_fn(|| Some((actions.next()?, actions.next()?)))
      .take(12)
      .map(|(key, label)| (key, label.chars().take(100).collect()))
      .collect();
    let hints = args.child_get::<HashMap<String, Variant>>(6);
    let icon = notification_image(&hints, &args.child_get::<String>(2));
    let timeout = args.child_get::<i32>(7);
    let duration = (timeout != 0)
      .then(|| Duration::from_millis(if timeout < 0 { 5500 } else { timeout as u64 }));
    let popup = !model.do_not_disturb;
    model.notices.push_front(Notice {
      id,
      summary: args.child_get::<String>(3).chars().take(256).collect(),
      body: args.child_get::<String>(4).chars().take(1024).collect(),
      actions,
      live: true,
      popup,
      seen: false,
      image: icon,
      transient: popup
        && hints
          .get("transient")
          .and_then(Variant::get)
          .unwrap_or(false),
      resident: hints
        .get("resident")
        .and_then(Variant::get)
        .unwrap_or(false),
      countdown: duration.map(Countdown::new),
    });
    drop(model);
    if let Some(old) = evicted {
      self.cancel_expiry(old.id);
      if old.live {
        self.closed_signal(old.id, 4);
      }
    }
    self.cancel_expiry(id);
    if let Some(duration) = duration {
      self.schedule_expiry(id, duration);
    }
    self.sync_hover();
    (self.refresh)();
    id
  }

  pub fn toggle_do_not_disturb(self: &Rc<Self>) {
    let mut model = self.model.borrow_mut();
    model.do_not_disturb = !model.do_not_disturb;
    if model.do_not_disturb {
      for notice in model.notices.iter_mut().filter(|notice| notice.popup) {
        notice.popup = false;
        notice.transient = false;
        if let Some(delay) = notice.countdown.as_mut().and_then(|countdown| {
          if countdown.expiry_delay().is_some() {
            return None;
          }
          countdown.resume();
          countdown.expiry_delay()
        }) {
          self.schedule_expiry(notice.id, delay);
        }
      }
    }
    drop(model);
    self.hover_popup();
  }

  fn cancel_expiry(&self, id: u32) {
    if let Some(timer) = self.expiry.borrow_mut().remove(&id) {
      timer.remove();
    }
  }

  fn schedule_expiry(self: &Rc<Self>, id: u32, delay: Duration) {
    let service = Rc::downgrade(self);
    let timer = glib::timeout_add_local_once(delay, move || {
      if let Some(service) = service.upgrade() {
        service.expiry.borrow_mut().remove(&id);
        service.close(id, 1);
      }
    });
    self.expiry.borrow_mut().insert(id, timer);
  }

  fn sync_hover(self: &Rc<Self>) {
    let mut model = self.model.borrow_mut();
    if model.popup_rows().next().is_none() {
      model.hover_popup(None);
    }
    let hovered = model.hovered_notice();
    for notice in model.notices.iter_mut().filter(|notice| notice.popup) {
      let Some(countdown) = &mut notice.countdown else {
        continue;
      };
      if hovered == Some(notice.id) {
        countdown.pause();
        self.cancel_expiry(notice.id);
      } else if countdown.expiry_delay().is_none() {
        countdown.resume();
        if let Some(delay) = countdown.expiry_delay() {
          self.schedule_expiry(notice.id, delay);
        }
      }
    }
  }

  pub fn hover_popup(self: &Rc<Self>) {
    self.sync_hover();
    (self.refresh)();
  }

  fn closed_signal(&self, id: u32, reason: u32) {
    let _ = self.bus.emit_signal(
      None,
      NOTIFY_PATH,
      NOTIFY,
      "NotificationClosed",
      Some(&(id, reason).to_variant()),
    );
  }

  pub fn close(self: &Rc<Self>, id: u32, reason: u32) {
    self.finish_notice(id, reason, reason == 2);
  }

  fn finish_notice(self: &Rc<Self>, id: u32, reason: u32, dismiss: bool) {
    let mut model = self.model.borrow_mut();
    let Some(index) = model.notices.iter().position(|notice| notice.id == id) else {
      return;
    };
    let notice = &mut model.notices[index];
    notice.popup = false;
    self.cancel_expiry(id);
    // Keep the sender's action available after the toast moves to history.
    if notice.live && (reason != 1 || notice.transient || notice.actions.is_empty()) {
      notice.live = false;
      self.closed_signal(id, reason);
    }
    if dismiss || notice.transient {
      model.notices.remove(index);
    }
    drop(model);
    self.sync_hover();
    (self.refresh)();
  }

  pub fn invoke(self: &Rc<Self>, id: u32, action: &str) {
    let mut model = self.model.borrow_mut();
    let Some(notice) = model.notices.iter_mut().find(|notice| notice.id == id) else {
      return;
    };
    if notice.live && notice.actions.iter().any(|(key, _)| key == action) {
      notice.seen = true;
      let resident = notice.resident;
      drop(model);
      let result = self.bus.emit_signal(
        None,
        NOTIFY_PATH,
        NOTIFY,
        "ActionInvoked",
        Some(&(id, action).to_variant()),
      );
      if result.is_ok() && !resident {
        self.finish_notice(id, 2, false);
      } else {
        (self.refresh)();
      }
    }
  }

  fn tray(self: &Rc<Self>) -> Result<()> {
    let interface = DBusNodeInfo::for_xml(TRAY_XML)?
      .lookup_interface(WATCHER)
      .expect("tray interface");
    let registration = self.clone();
    let query = self.clone();
    self
      .bus
      .register_object(WATCHER_PATH, &interface)
      .method_call(move |_, sender, _, _, method, args, invocation| {
        if method == "RegisterStatusNotifierItem"
          && let Some(sender) = sender
        {
          registration.add_item(sender, &args.child_get::<String>(0));
        }
        invocation.return_value(None);
      })
      .property(move |_, _, _, _, property| match property {
        "RegisteredStatusNotifierItems" => query
          .items
          .borrow()
          .iter()
          .map(|i| item_id(&i.address))
          .collect::<Vec<_>>()
          .to_variant(),
        "IsStatusNotifierHostRegistered" => true.to_variant(),
        _ => 0i32.to_variant(),
      })
      .build()?;

    let service = self.clone();
    let names = self.bus.subscribe_to_signal(
      Some(BUS),
      Some(BUS),
      Some("NameOwnerChanged"),
      Some(BUS_PATH),
      None,
      DBusSignalFlags::NONE,
      move |signal| {
        if let Some((name, previous, _)) = signal.parameters.get::<(String, String, String)>()
          && !previous.is_empty()
        {
          service.remove_items(&name, &previous);
        }
      },
    );
    let service = self.clone();
    let changes =
      self.bus.subscribe_to_signal(
        None,
        Some(ITEM),
        None,
        None,
        None,
        DBusSignalFlags::NONE,
        move |signal| {
          if let Some(item) = service.items.borrow().iter().find(|item| {
            item.owner == signal.sender_name && item.address.path == signal.object_path
          }) {
            service.refresh_item(&item.address);
          }
        },
      );
    self.subscriptions.borrow_mut().extend([names, changes]);
    let _ = self.bus.emit_signal(
      None,
      WATCHER_PATH,
      WATCHER,
      "StatusNotifierHostRegistered",
      None,
    );
    Ok(())
  }

  fn add_item(self: &Rc<Self>, sender: &str, service: &str) {
    let (name, path) = if service.starts_with('/') {
      (sender.to_owned(), service.to_owned())
    } else {
      (service.to_owned(), "/StatusNotifierItem".to_owned())
    };
    if !gio::dbus_is_name(&name) || !Variant::is_object_path(&path) {
      return;
    }
    let address = Address { name, path };
    if address.name == sender {
      self.register_item(address, sender.to_owned());
    } else {
      let owner = sender.to_owned();
      let shell = self.clone();
      self.bus.call(
        Some(BUS),
        BUS_PATH,
        BUS,
        "GetNameOwner",
        Some(&(&address.name,).to_variant()),
        None,
        DBusCallFlags::NONE,
        1500,
        None::<&Cancellable>,
        move |reply| {
          if reply.ok().and_then(|v| v.get::<(String,)>()) == Some((owner.clone(),)) {
            shell.register_item(address, owner);
          }
        },
      );
    }
  }

  fn register_item(self: &Rc<Self>, address: Address, owner: String) {
    let mut items = self.items.borrow_mut();
    if items.len() >= TRAY_LIMIT || items.iter().any(|item| item.address == address) {
      return;
    }
    items.push(RegisteredItem {
      address: address.clone(),
      owner,
    });
    drop(items);
    let _ = self.bus.emit_signal(
      None,
      WATCHER_PATH,
      WATCHER,
      "StatusNotifierItemRegistered",
      Some(&(item_id(&address),).to_variant()),
    );
    self.refresh_item(&address);
  }

  fn remove_items(&self, name: &str, owner: &str) {
    let mut records = self.items.borrow_mut();
    let mut removed = Vec::new();
    records.retain(|item| {
      if item.address.name == name || (name.starts_with(':') && item.owner == owner) {
        removed.push(item.address.clone());
        false
      } else {
        true
      }
    });
    if removed.is_empty() {
      return;
    }
    self
      .model
      .borrow_mut()
      .trays
      .retain(|item| !removed.contains(&item.address));
    for addr in removed {
      let _ = self.bus.emit_signal(
        None,
        WATCHER_PATH,
        WATCHER,
        "StatusNotifierItemUnregistered",
        Some(&(item_id(&addr),).to_variant()),
      );
    }
    (self.refresh)();
  }

  fn refresh_item(self: &Rc<Self>, addr: &Address) {
    let shell = self.clone();
    let address = addr.clone();
    self.bus.call(
      Some(&addr.name),
      &addr.path,
      PROPS,
      "GetAll",
      Some(&(ITEM,).to_variant()),
      None,
      DBusCallFlags::NONE,
      1500,
      None::<&Cancellable>,
      move |reply| {
        let Ok(reply) = reply else { return };
        let props: HashMap<String, Variant> = reply.child_get(0);
        if !shell
          .items
          .borrow()
          .iter()
          .any(|item| item.address == address)
        {
          return;
        }
        let property = |key| props.get(key).and_then(Variant::get::<String>);
        let mut model = shell.model.borrow_mut();
        let index = model.trays.iter().position(|item| item.address == address);
        if property("Status").as_deref() == Some("Passive") {
          if let Some(index) = index {
            model.trays.remove(index);
          }
        } else {
          let title = property("Title")
            .filter(|title| !title.is_empty())
            .or_else(|| property("Id"))
            .unwrap_or_else(|| address.name.clone());
          let pixmaps = if property("Status").as_deref() == Some("NeedsAttention") {
            props
              .get("AttentionIconPixmap")
              .or_else(|| props.get("IconPixmap"))
          } else {
            props.get("IconPixmap")
          };
          let item = TrayItem {
            address,
            title: title.chars().take(150).collect(),
            image: pixmaps.and_then(tray_icon),
            menu_only: props
              .get("ItemIsMenu")
              .and_then(Variant::get::<bool>)
              .unwrap_or(false),
          };
          if let Some(index) = index {
            model.trays[index] = item;
          } else {
            model.trays.push(item);
          }
        }
        drop(model);
        (shell.refresh)();
      },
    );
  }

  pub fn tray_action(&self, address: &Address, action: &str) {
    self.bus.call(
      Some(&address.name),
      &address.path,
      ITEM,
      action,
      Some(&(0i32, 0i32).to_variant()),
      None,
      DBusCallFlags::NONE,
      1500,
      None::<&Cancellable>,
      |_| {},
    );
  }
}

fn item_id(address: &Address) -> String {
  format!("{}{}", address.name, address.path)
}

fn tray_icon(pixmaps: &Variant) -> Option<cairo::ImageSurface> {
  let (width, height, mut pixels) = pixmaps
    .get::<Vec<(i32, i32, Vec<u8>)>>()?
    .into_iter()
    .filter(|(w, h, p)| {
      (1..=256).contains(w) && (1..=256).contains(h) && p.len() == (w * h * 4) as usize
    })
    .min_by_key(|(w, h, _)| (32 - (*w).min(*h)).abs())?;
  for pixel in pixels.chunks_exact_mut(4) {
    let a = pixel[0];
    let channel = |c| (u16::from(c) * u16::from(a) / 255) as u8;
    let bgra = [channel(pixel[3]), channel(pixel[2]), channel(pixel[1]), a];
    pixel.copy_from_slice(&bgra);
  }
  image(Art {
    data: pixels,
    width,
    height,
  })
}
