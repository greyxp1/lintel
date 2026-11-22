use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use serde_json::Value;

pub const MUSIC_SINK: &str = "desktop_shell_music_sink";
const SEND_CAPTURE: &str = "desktop_shell_music_send_capture";

pub fn music_mix_enabled() -> bool {
  std::env::var_os("LINTEL_MUSIC_MIX").is_some_and(|value| value == "1")
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Device {
  Output,
  Input,
}

impl Device {
  fn target(self) -> &'static str {
    match self {
      Self::Output => "@DEFAULT_AUDIO_SINK@",
      Self::Input => "@DEFAULT_AUDIO_SOURCE@",
    }
  }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Volume {
  pub percent: u8,
  pub muted: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MusicChannel {
  Listen,
  Send,
}

impl MusicChannel {
  pub const ALL: [Self; 2] = [Self::Listen, Self::Send];

  fn node(self) -> &'static str {
    match self {
      Self::Listen => "desktop_shell_music_listen",
      Self::Send => "desktop_shell_music_send",
    }
  }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct StreamId {
  index: u32,
  pub recording: bool,
}

#[derive(PartialEq, Eq)]
pub struct Stream {
  pub id: StreamId,
  pub members: Vec<StreamId>,
  pub name: String,
  pub icon: String,
  pub volume: Volume,
  pub music: bool,
}

pub fn streams(player: u32, music_only: bool, split_music: bool) -> Vec<Stream> {
  let music_inputs = if split_music {
    music_stream_ids().unwrap_or_default()
  } else {
    Vec::new()
  };
  let mut streams: Vec<Stream> = Vec::new();
  for (kind, recording) in [("sink-inputs", false), ("source-outputs", true)] {
    if music_only && (recording || player == 0) {
      break;
    }
    let Ok(output) = Command::new("pactl")
      .args(["--format=json", "list", kind])
      .output()
    else {
      continue;
    };
    let Ok(items) = serde_json::from_slice::<Vec<Value>>(&output.stdout) else {
      continue;
    };
    for stream in items.into_iter().filter_map(|item| {
      let index = u32::try_from(item["index"].as_u64()?).ok()?;
      let properties = &item["properties"];
      let music = !recording
        && (music_inputs.contains(&u64::from(index))
          || (player != 0
            && properties["application.process.id"]
              .as_str()
              .and_then(|pid| pid.parse().ok())
              == Some(player)));
      if music_only && !music {
        return None;
      }
      if properties["node.passive"].as_str() == Some("true")
        || (item["client"].is_null() && properties["media.role"].as_str() == Some("production"))
      {
        return None;
      }
      let label = ["application.name", "media.name", "node.name"]
        .into_iter()
        .find_map(|key| properties[key].as_str().filter(|name| !name.is_empty()))?;
      let percent = item["volume"]
        .as_object()?
        .values()
        .filter_map(|channel| {
          channel["value_percent"]
            .as_str()?
            .trim_end_matches('%')
            .parse()
            .ok()
        })
        .max()?;
      let icon = properties["application.icon_name"]
        .as_str()
        .or_else(|| properties["application.id"].as_str())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| label.to_ascii_lowercase());
      let id = StreamId { index, recording };
      Some(Stream {
        id,
        members: vec![id],
        name: label.to_owned(),
        icon,
        volume: Volume {
          percent,
          muted: item["mute"].as_bool().unwrap_or(false),
        },
        music,
      })
    }) {
      if let Some(group) = streams.iter_mut().find(|group| {
        group.id.recording == recording
          && group.music == stream.music
          && (stream.music || group.name.eq_ignore_ascii_case(&stream.name))
      }) {
        group.members.push(stream.id);
        if stream.id.index < group.id.index {
          group.id = stream.id;
        }
        group.volume.percent = group.volume.percent.max(stream.volume.percent);
        group.volume.muted &= stream.volume.muted;
      } else {
        streams.push(stream);
      }
    }
  }
  streams.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.index.cmp(&b.id.index)));
  streams
}

pub fn set_stream(id: StreamId, value: Option<u8>, muted: bool) -> io::Result<()> {
  let kind = if id.recording {
    "source-output"
  } else {
    "sink-input"
  };
  let (property, value) = match value {
    Some(volume) => ("volume", format!("{volume}%")),
    None => ("mute", if muted { "1" } else { "0" }.into()),
  };
  let status = Command::new("pactl")
    .args([
      format!("set-{kind}-{property}"),
      id.index.to_string(),
      value,
    ])
    .status()?;
  if !status.success() {
    return Err(io::Error::other("Application audio stream unavailable"));
  }
  Ok(())
}

pub fn get(device: Device) -> io::Result<Volume> {
  get_target(device.target())
}

fn get_target(target: &str) -> io::Result<Volume> {
  let output = Command::new("wpctl")
    .args(["get-volume", target])
    .output()?;
  let line = String::from_utf8_lossy(&output.stdout);
  let percent = line
    .split_whitespace()
    .nth(1)
    .and_then(|number| number.parse::<f32>().ok())
    .filter(|_| output.status.success())
    .ok_or_else(|| io::Error::other("Audio device unavailable"))?;
  Ok(Volume {
    percent: (100.0 * percent).round() as u8,
    muted: line.contains("[MUTED]"),
  })
}

pub fn set(device: Device, value: Option<u8>) -> io::Result<Volume> {
  set_target(device.target(), value)
}

fn set_target(target: &str, value: Option<u8>) -> io::Result<Volume> {
  let mut command = Command::new("wpctl");
  if let Some(value) = value {
    command.args(["set-volume", target, &format!("{value}%")]);
  } else {
    command.args(["set-mute", target, "toggle"]);
  }
  if !command.status()?.success() {
    return Err(io::Error::other("Could not set audio volume"));
  }
  get_target(target)
}

pub fn set_music(node: u64, value: Option<u8>) -> io::Result<Volume> {
  set_target(&node.to_string(), value)
}

pub fn music_levels() -> [Option<(u64, Volume)>; 2] {
  let Ok(objects) = graph() else {
    return [None; 2];
  };
  MusicChannel::ALL.map(|channel| {
    let object = find_node(&objects, channel.node())?;
    let node = id(object)?;
    let props = &object["info"]["params"]["Props"][0];
    let gain = props["channelVolumes"]
      .as_array()?
      .iter()
      .filter_map(Value::as_f64)
      .reduce(f64::max)?;
    Some((
      node,
      Volume {
        // WirePlumber's displayed gain is cubic.
        percent: (gain.cbrt() * 100.0).round() as u8,
        muted: props["mute"].as_bool().unwrap_or(false),
      },
    ))
  })
}

pub fn step_output(step: i32) -> io::Result<Volume> {
  let value = format!(
    "{}%{}",
    step.unsigned_abs(),
    if step > 0 { '+' } else { '-' }
  );
  let mut command = Command::new("wpctl");
  command.arg("set-volume");
  if step > 0 {
    command.args(["-l", "1"]);
  }
  let status = command.args([Device::Output.target(), &value]).status()?;
  if !status.success() {
    return Err(io::Error::other("Could not set output volume"));
  }
  get(Device::Output)
}

pub enum Route {
  Update { enabled: bool, revision: u64 },
  Stop,
}

pub struct RouteStatus {
  pub enabled: bool,
  pub revision: u64,
  pub error: Option<String>,
}

pub fn route_worker() -> (
  Sender<Route>,
  async_channel::Receiver<RouteStatus>,
  thread::JoinHandle<()>,
) {
  let (send, receive) = mpsc::channel();
  let (report, updates) = async_channel::bounded(8);
  let worker = thread::spawn(move || {
    let mut links = Vec::new();
    let mut active = None;
    let mut retry: u8 = 0;
    loop {
      let route = if retry == 0 {
        receive.recv().map_err(Into::into)
      } else {
        receive.recv_timeout(Duration::from_millis(275))
      };
      match route {
        Ok(Route::Stop) | Err(RecvTimeoutError::Disconnected) => return,
        Ok(Route::Update {
          enabled: false,
          revision,
        }) => {
          retry = 0;
          active = None;
          links.clear();
          let _ = report.try_send(RouteStatus {
            enabled: false,
            revision,
            error: None,
          });
        }
        Ok(Route::Update {
          enabled: true,
          revision,
        }) => {
          active = Some(revision);
          retry = 12;
        }
        Err(RecvTimeoutError::Timeout) => {}
      }
      if let Some(revision) = active {
        match sync_links(&mut links) {
          Ok(ready) => {
            if retry == 12 {
              let _ = report.try_send(RouteStatus {
                enabled: true,
                revision,
                error: None,
              });
            }
            retry = if ready { 0 } else { retry.saturating_sub(1) };
          }
          Err(error) => {
            links.clear();
            let _ = report.try_send(RouteStatus {
              enabled: false,
              revision,
              error: Some(error.to_string()),
            });
            active = None;
            retry = 0;
          }
        }
      }
    }
  });
  (send, updates, worker)
}

fn graph() -> io::Result<Vec<Value>> {
  let output = Command::new("pw-dump").output()?;
  if !output.status.success() {
    return Err(io::Error::other("PipeWire is unavailable"));
  }
  serde_json::from_slice(&output.stdout).map_err(io::Error::other)
}

fn props(object: &Value) -> &Value {
  &object["info"]["props"]
}

fn id(object: &Value) -> Option<u64> {
  object["id"].as_u64()
}

fn find_node<'a>(objects: &'a [Value], name: &str) -> Option<&'a Value> {
  objects.iter().find(|object| {
    object["type"] == "PipeWire:Interface:Node" && props(object)["node.name"] == name
  })
}

fn music_stream_ids() -> Option<Vec<u64>> {
  let objects = graph().ok()?;
  let sink = id(find_node(&objects, MUSIC_SINK)?)?;
  Some(
    objects
      .iter()
      .filter_map(|object| {
        let from = object["info"]["output-node-id"].as_u64()?;
        if object["info"]["input-node-id"].as_u64()? != sink {
          return None;
        }
        // PipeWire-Pulse exposes the stream's serial as its sink-input index.
        let stream = objects.iter().find(|node| id(node) == Some(from))?;
        props(stream)["object.serial"].as_u64()
      })
      .collect(),
  )
}

struct MusicLink {
  ports: (u64, u64),
  helper: Child,
}

impl MusicLink {
  fn connect(ports: (u64, u64)) -> io::Result<Self> {
    let mut command = Command::new("pw-link");
    command
      .args([
        "--monitor",
        "--passive",
        &ports.0.to_string(),
        &ports.1.to_string(),
      ])
      .stdin(Stdio::null())
      .stdout(Stdio::null());
    let parent = std::process::id() as libc::pid_t;
    // Monitor mode owns the link until it disconnects, including when the shell crashes.
    unsafe {
      command.pre_exec(move || {
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
          return Err(io::Error::last_os_error());
        }
        if libc::getppid() != parent {
          return Err(io::Error::from_raw_os_error(libc::ESRCH));
        }
        Ok(())
      });
    }
    Ok(Self {
      ports,
      helper: command.spawn()?,
    })
  }
}

impl Drop for MusicLink {
  fn drop(&mut self) {
    let _ = self.helper.kill();
    let _ = self.helper.wait();
  }
}

fn sync_links(links: &mut Vec<MusicLink>) -> io::Result<bool> {
  let objects = graph()?;
  let sink = find_node(&objects, SEND_CAPTURE)
    .and_then(id)
    .ok_or_else(|| io::Error::other("Music microphone is unavailable"))?;
  let source = find_node(&objects, MUSIC_SINK)
    .and_then(id)
    .ok_or_else(|| io::Error::other("Music output is unavailable"))?;
  let port = |node, direction: &str, channel: &str| {
    objects
      .iter()
      .find(|object| {
        object["type"] == "PipeWire:Interface:Port"
          && props(object)["node.id"].as_u64() == Some(node)
          && props(object)["port.direction"] == direction
          && props(object)["audio.channel"] == channel
          && (direction != "out" || props(object)["port.monitor"].as_bool() == Some(true))
      })
      .and_then(id)
  };
  let channels = ["FL", "FR"]
    .into_iter()
    .map(|channel| Some((port(source, "out", channel)?, port(sink, "in", channel)?)))
    .collect::<Option<Vec<_>>>();
  let Some(channels) = channels else {
    links.clear();
    return Ok(false);
  };
  links.retain(|link| channels.contains(&link.ports));
  let mut ready = true;
  for ports in channels {
    let connected = objects.iter().any(|object| {
      object["type"] == "PipeWire:Interface:Link"
        && object["info"]["output-port-id"] == ports.0
        && object["info"]["input-port-id"] == ports.1
    });
    if !connected {
      ready = false;
      if let Some(link) = links.iter_mut().find(|link| link.ports == ports) {
        if link.helper.try_wait()?.is_some() {
          return Err(io::Error::other("Could not send music to the microphone"));
        }
      } else {
        links.push(MusicLink::connect(ports)?);
      }
    }
  }
  Ok(ready)
}
