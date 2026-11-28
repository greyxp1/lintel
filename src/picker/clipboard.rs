use super::{Haystack, copy, run};
use crate::media::{Art, thumbnail_art};

pub struct Clip {
  pub id: u64,
  pub text: String,
  /// Format and dimensions, then size, for PNG, JPEG and BMP entries.
  pub image: Option<(String, String)>,
  pub search: Haystack,
}

pub fn list() -> Result<Vec<Clip>, String> {
  let output = run("cliphist", &["-preview-width", "240", "list"], None)?;
  Ok(
    String::from_utf8_lossy(&output)
      .lines()
      .filter_map(|line| {
        let (id, text) = line.split_once('\t')?;
        // Browsers store a text/html copy next to copied images.
        if text.starts_with("<meta http-equiv=") {
          return None;
        }
        let image = text
          .strip_prefix("[[ binary data ")
          .and_then(|info| info.strip_suffix(" ]]"))
          .and_then(|info| {
            let mut words = info.split_whitespace();
            let size = format!("{} {}", words.next()?, words.next()?);
            let format = words.next()?;
            if !matches!(format, "png" | "jpg" | "jpeg" | "bmp") {
              return None;
            }
            let dimensions = words.next().unwrap_or_default().replace('x', "×");
            Some((format!("{} {dimensions}", format.to_uppercase()), size))
          });
        Some(Clip {
          id: id.parse().ok()?,
          search: Haystack::new([(text, 1, true)]),
          text: text.to_owned(),
          image,
        })
      })
      .collect(),
  )
}

fn decode(id: u64) -> Result<Vec<u8>, String> {
  run(
    "cliphist",
    &["decode"],
    Some(format!("{id}\t\n").as_bytes()),
  )
}

pub fn thumbnail(id: u64, width: u32, height: u32) -> Option<Art> {
  let image = image::load_from_memory(&decode(id).ok()?).ok()?;
  Some(thumbnail_art(image, width, height))
}

pub fn select(id: u64) -> Result<(), String> {
  copy(&decode(id)?, false)
}

pub fn delete(id: u64) -> Result<(), String> {
  run(
    "cliphist",
    &["delete"],
    Some(format!("{id}\t\n").as_bytes()),
  )
  .map(drop)
}

pub fn wipe() -> Result<(), String> {
  run("cliphist", &["wipe"], None).map(drop)
}
