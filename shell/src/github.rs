use std::collections::{BTreeMap, HashMap};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use gio::glib::{self, MainContext};
use serde::{Deserialize, Serialize};

use crate::media::Art;
use crate::services::Services;
use crate::ui::{Notice, image};

const SEEN_LIMIT: usize = 250;
const MIN_INTERVAL: u64 = 60;
const MAX_BACKOFF: u64 = 15 * 60;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct State {
  last_modified: Option<String>,
  etag: Option<String>,
  cutoff: Option<String>,
  seen: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Thread {
  id: String,
  reason: String,
  updated_at: String,
  subject: Subject,
  repository: Repository,
}

#[derive(Clone, Deserialize)]
struct Subject {
  title: String,
  url: Option<String>,
  latest_comment_url: Option<String>,
}

#[derive(Deserialize)]
struct Repository {
  full_name: String,
  html_url: String,
  owner: Owner,
}

#[derive(Deserialize)]
struct Owner {
  avatar_url: String,
}

struct Update {
  id: String,
  title: String,
  body: String,
  links: Links,
  avatar: Option<Art>,
}

#[derive(Clone)]
struct Links {
  subject: Subject,
  repository: String,
}

pub fn watch(services: Rc<Services>) {
  let (send, receive) = async_channel::bounded::<Update>(64);
  let spawned = thread::Builder::new()
    .name("github".into())
    .spawn(move || poll(&send));
  if let Err(error) = spawned {
    eprintln!("GitHub notifications: {error}");
    return;
  }
  MainContext::default().spawn_local(async move {
    while let Ok(update) = receive.recv().await {
      let notice = Notice {
        summary: update.title,
        body: update.body,
        source: Some("github.com".to_owned()),
        actions: vec![
          ("default".to_owned(), "Open".to_owned()),
          ("read".to_owned(), "Mark read".to_owned()),
        ],
        image: update.avatar.and_then(image),
        ..Notice::default()
      };
      let id = update.id.clone();
      let links = update.links;
      services.notify_local(format!("github:{id}"), notice, move |action| {
        let (id, links, open) = (id.clone(), links.clone(), action == "default");
        let _ = thread::Builder::new()
          .name("github-action".into())
          .spawn(move || {
            if open {
              open_url(&web_url(&links));
            }
            mark_read(&id);
          });
      });
    }
  });
}

fn open_url(url: &str) {
  if !url.starts_with("https://") {
    return;
  }
  match Command::new("xdg-open")
    .arg(url)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
  {
    Ok(mut child) => {
      let _ = thread::Builder::new()
        .name("github-open".into())
        .spawn(move || {
          let _ = child.wait();
        });
    }
    Err(error) => eprintln!("Open GitHub notification: {error}"),
  }
}

fn state_path() -> PathBuf {
  glib::user_state_dir().join("lintel/github-notifications.json")
}

fn save(state: &State) {
  use gio::glib::{FileSetContentsFlags, file_set_contents_full};
  let path = state_path();
  let result: Result<(), Box<dyn std::error::Error>> = (|| {
    std::fs::create_dir_all(path.parent().unwrap())?;
    let contents = serde_json::to_vec(state)?;
    file_set_contents_full(&path, &contents, FileSetContentsFlags::CONSISTENT, 0o600)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
  })();
  if let Err(error) = result {
    eprintln!("Save GitHub notification state: {error}");
  }
}

fn poll(send: &async_channel::Sender<Update>) {
  let mut state = std::fs::read(state_path())
    .ok()
    .and_then(|bytes| serde_json::from_slice::<State>(&bytes).ok());
  let mut avatars = HashMap::<String, Option<Art>>::new();
  let mut backoff = MIN_INTERVAL;
  let mut last_error = String::new();
  loop {
    let delay = match fetch(
      state
        .as_ref()
        .and_then(|state| state.last_modified.as_deref()),
      state.as_ref().and_then(|state| state.etag.as_deref()),
    ) {
      Ok(response) => {
        last_error.clear();
        backoff = MIN_INTERVAL;
        if let Some(threads) = response.threads {
          let seeded = state.is_some();
          let current = state.get_or_insert_default();
          if !seeded {
            current.cutoff = glib::DateTime::now_utc()
              .ok()
              .and_then(|now| now.format("%Y-%m-%dT%H:%M:%SZ").ok())
              .map(|text| text.to_string());
          }
          // The first poll only seeds the seen threads so the existing backlog stays quiet.
          for thread in threads.iter().rev() {
            let after_cutoff = current
              .cutoff
              .as_deref()
              .is_none_or(|cutoff| thread.updated_at.as_str() > cutoff);
            let changed = current
              .seen
              .get(&thread.id)
              .is_none_or(|seen| *seen < thread.updated_at);
            if seeded
              && after_cutoff
              && changed
              && send.send_blocking(update(thread, &mut avatars)).is_err()
            {
              return;
            }
          }
          for thread in &threads {
            current
              .seen
              .insert(thread.id.clone(), thread.updated_at.clone());
          }
          trim(&mut current.seen);
          current.last_modified = response.last_modified;
          current.etag = response.etag;
          save(current);
        }
        response.interval.max(MIN_INTERVAL)
      }
      Err(error) => {
        if error != last_error {
          eprintln!("GitHub notifications: {error}");
          last_error = error;
        }
        let delay = backoff;
        backoff = (backoff * 2).min(MAX_BACKOFF);
        delay
      }
    };
    thread::sleep(Duration::from_secs(delay));
  }
}

fn trim(seen: &mut BTreeMap<String, String>) {
  if seen.len() <= SEEN_LIMIT {
    return;
  }
  let mut entries: Vec<_> = std::mem::take(seen).into_iter().collect();
  entries.sort_unstable_by(|a, b| b.1.cmp(&a.1));
  entries.truncate(SEEN_LIMIT);
  seen.extend(entries);
}

fn update(thread: &Thread, avatars: &mut HashMap<String, Option<Art>>) -> Update {
  let url = &thread.repository.owner.avatar_url;
  if avatars.len() >= 64 && !avatars.contains_key(url) {
    avatars.clear();
  }
  let avatar = avatars
    .entry(url.clone())
    .or_insert_with(|| {
      let separator = if url.contains('?') { '&' } else { '?' };
      crate::media::artwork(&format!("{url}{separator}s=96"), 48)
    })
    .clone();
  let reason = match thread.reason.as_str() {
    "assign" => "assigned".to_owned(),
    "ci_activity" => "CI activity".to_owned(),
    "manual" => "subscribed".to_owned(),
    "author" => "your thread".to_owned(),
    reason => reason.replace('_', " "),
  };
  Update {
    id: thread.id.clone(),
    title: thread.subject.title.chars().take(256).collect(),
    body: format!("{}: {reason}", thread.repository.full_name),
    links: Links {
      subject: thread.subject.clone(),
      repository: thread.repository.html_url.clone(),
    },
    avatar,
  }
}

struct Response {
  threads: Option<Vec<Thread>>,
  last_modified: Option<String>,
  etag: Option<String>,
  interval: u64,
}

fn fetch(since: Option<&str>, etag: Option<&str>) -> Result<Response, String> {
  let mut args = vec!["api", "--include"];
  let since_header;
  if let Some(since) = since {
    since_header = format!("If-Modified-Since: {since}");
    args.extend(["-H", &since_header]);
  }
  let etag_header;
  if let Some(etag) = etag {
    etag_header = format!("If-None-Match: {etag}");
    args.extend(["-H", &etag_header]);
  }
  args.push("/notifications?per_page=50");
  let output = gh(&args)?;
  let text = String::from_utf8_lossy(&output.stdout);
  let mut lines = text.split_inclusive('\n');
  let first = lines.next().unwrap_or_default();
  let status = first
    .split_whitespace()
    .nth(1)
    .and_then(|code| code.parse::<u16>().ok());
  let error = || {
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr
      .lines()
      .next()
      .unwrap_or("gh api failed")
      .trim()
      .to_owned()
  };
  let mut headers = HashMap::new();
  let mut offset = first.len();
  for line in lines {
    offset += line.len();
    let line = line.trim_end();
    if line.is_empty() {
      break;
    }
    if let Some((name, value)) = line.split_once(':') {
      headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
    }
  }
  let interval = headers
    .get("x-poll-interval")
    .and_then(|value| value.parse().ok())
    .unwrap_or(MIN_INTERVAL);
  let threads = match status {
    Some(304) => None,
    Some(200) => Some(
      serde_json::from_str(&text[offset..])
        .map_err(|error| format!("invalid response: {error}"))?,
    ),
    _ => return Err(error()),
  };
  Ok(Response {
    threads,
    last_modified: headers.remove("last-modified"),
    etag: headers.remove("etag"),
    interval,
  })
}

fn gh(args: &[&str]) -> Result<Output, String> {
  let child = Command::new("gh")
    .args(args)
    .env("GH_PROMPT_DISABLED", "1")
    .env("GH_NO_UPDATE_NOTIFIER", "1")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .map_err(|error| format!("gh: {error}"))?;
  let pid = rustix::process::Pid::from_child(&child);
  let (send, receive) = mpsc::channel();
  thread::spawn(move || send.send(child.wait_with_output()));
  let result = receive.recv_timeout(COMMAND_TIMEOUT).or_else(|_| {
    let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
    receive.recv()
  });
  result
    .map_err(|_| "gh did not finish".to_owned())?
    .map_err(|error| format!("gh: {error}"))
}

fn web_url(links: &Links) -> String {
  let mut api = [&links.subject.latest_comment_url, &links.subject.url]
    .into_iter()
    .flatten()
    .filter(|url| url.starts_with("https://api.github.com/"));
  for url in api.clone() {
    if let Ok(output) = gh(&["api", url, "--jq", ".html_url"])
      && output.status.success()
      && let Some(html) = String::from_utf8_lossy(&output.stdout).lines().next()
      && html.starts_with("https://")
    {
      return html.to_owned();
    }
  }
  // Fall back to the web path that mirrors the API path when the lookup fails.
  api
    .next_back()
    .and_then(|url| url.strip_prefix("https://api.github.com/repos/"))
    .map(|path| {
      let path = path
        .replacen("/pulls/", "/pull/", 1)
        .replacen("/commits/", "/commit/", 1);
      format!("https://github.com/{path}")
    })
    .unwrap_or_else(|| links.repository.clone())
}

fn mark_read(id: &str) {
  if !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()) {
    match gh(&[
      "api",
      "-X",
      "PATCH",
      &format!("/notifications/threads/{id}"),
    ]) {
      Ok(output) if output.status.success() => {}
      Ok(output) => {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if let Some(line) = stderr.lines().next() {
          eprintln!("Mark GitHub notification read: {}", line.trim());
        }
      }
      Err(error) => eprintln!("Mark GitHub notification read: {error}"),
    }
  }
}
