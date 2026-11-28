use std::process::{Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use super::{Haystack, copy, run};

// Typing starts once the compositor confirms the unmap, or after this if it never does.
const UNMAP_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Clone)]
pub struct Login {
  pub id: String,
  pub name: String,
  pub user: String,
  pub search: Haystack,
}

#[derive(Clone, Copy)]
pub enum Action {
  Login,
  Password,
  Username,
  Code,
}

pub const ACTIONS: [(Action, &str); 4] = [
  (Action::Login, "Type username and password"),
  (Action::Password, "Type password"),
  (Action::Username, "Type username"),
  (Action::Code, "Copy one-time code"),
];

pub fn unlocked() -> bool {
  Command::new("rbw")
    .arg("unlocked")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .status()
    .is_ok_and(|status| status.success())
}

pub fn unlock() -> Result<(), String> {
  run("rbw", &["unlock"], None).map(drop)
}

pub fn sync() {
  let _ = run("rbw", &["sync"], None);
}

pub fn list() -> Result<Vec<Login>, String> {
  let output = run("rbw", &["list", "--fields", "id,name,user"], None)?;
  Ok(
    String::from_utf8_lossy(&output)
      .lines()
      .filter_map(|line| {
        let mut fields = line.splitn(3, '\t');
        let id = fields.next().filter(|id| !id.is_empty())?.to_owned();
        let name = fields.next().unwrap_or_default().to_owned();
        let user = fields.next().unwrap_or_default().to_owned();
        Some(Login {
          search: Haystack::new([(name.as_str(), 2, true), (user.as_str(), 1, true)]),
          id,
          name,
          user,
        })
      })
      .collect(),
  )
}

pub fn perform(action: Action, id: &str, unmapped: &Receiver<()>) -> Result<(), String> {
  let field = |username: bool| {
    let args: &[&str] = if username {
      &["get", "--field", "username", id]
    } else {
      &["get", id]
    };
    let value = run("rbw", args, None)?;
    Ok::<_, String>(
      String::from_utf8_lossy(&value)
        .trim_end_matches('\n')
        .to_owned(),
    )
  };
  let write = |text: &str| run("wtype", &["-"], Some(text.as_bytes())).map(drop);
  let settle = || {
    let _ = unmapped.recv_timeout(UNMAP_TIMEOUT);
  };
  match action {
    Action::Login => {
      let (user, password) = (field(true)?, field(false)?);
      settle();
      write(&user)?;
      run("wtype", &["-k", "Tab"], None)?;
      write(&password)
    }
    Action::Password | Action::Username => {
      let value = field(matches!(action, Action::Username))?;
      settle();
      write(&value)
    }
    Action::Code => copy(&run("rbw", &["code", id], None)?, true),
  }
}
