use cairo::{Context, ImageSurface};
use pango::{Alignment, Weight};

use super::{Mode, Picker, Status, Step, View, vault};
use crate::ui::{color, label, label_lines, picture, round, search_symbol, text_layout};

pub const WIDTH: i32 = 640;
pub const HEIGHT: i32 = LIST_Y + LIST_HEIGHT + PAD;
pub const RADIUS: f64 = 14.0;
pub(super) const ICON: i32 = 28;
const PAD: i32 = 10;
const FIELD: i32 = 44;
const LIST_Y: i32 = PAD + FIELD + 8;
const LIST_HEIGHT: i32 = 440;
const ROW_WIDTH: i32 = WIDTH - 2 * PAD;
const TEXT_X: i32 = PAD + 12;
const THUMBNAIL: (i32, i32) = (144, 72);
const BUTTON: (i32, i32) = (110, 34);
const BUTTON_Y: i32 = LIST_Y + 196;
const TEXT: u32 = 0xcdd6f4;
const BRIGHT: u32 = 0xf5f3ff;
const DIM: u32 = 0xa6adc8;
const ACCENT: u32 = 0xcba6f7;
const ERROR: u32 = 0xf38ba8;

fn row_height(view: &View) -> i32 {
  if view.mode == Mode::Clipboard && matches!(view.step, Step::List) {
    88
  } else {
    44
  }
}

pub(super) fn rows(view: &View) -> usize {
  (LIST_HEIGHT / row_height(view)) as usize
}

pub(super) fn row_at(view: &View, x: f64, y: f64) -> Option<usize> {
  if !(f64::from(PAD)..f64::from(PAD + ROW_WIDTH)).contains(&x) || y < f64::from(LIST_Y) {
    return None;
  }
  let row = ((y - f64::from(LIST_Y)) / f64::from(row_height(view))) as usize;
  (row < rows(view))
    .then_some(view.offset + row)
    .filter(|&index| index < view.matches.len())
}

pub(super) fn thumbnail_size(scale: i32) -> (u32, u32) {
  ((THUMBNAIL.0 * scale) as u32, (THUMBNAIL.1 * scale) as u32)
}

fn buttons() -> [(i32, &'static str); 2] {
  [
    (WIDTH / 2 - 6 - BUTTON.0, "Cancel"),
    (WIDTH / 2 + 6, "Wipe"),
  ]
}

/// Whether a click landed on the confirming or the cancelling wipe button.
pub(super) fn wipe_button(x: f64, y: f64) -> Option<bool> {
  if !(f64::from(BUTTON_Y)..f64::from(BUTTON_Y + BUTTON.1)).contains(&y) {
    return None;
  }
  buttons()
    .into_iter()
    .position(|(left, _)| (f64::from(left)..f64::from(left + BUTTON.0)).contains(&x))
    .map(|index| index == 1)
}

pub fn paint(cr: &Context, picker: &Picker, opacity: f64) -> cairo::Result<()> {
  let Some(view) = &picker.view else {
    return Ok(());
  };
  round(cr, 0, 0, WIDTH, HEIGHT, RADIUS);
  color(cr, 0x1e1e2e, opacity);
  cr.fill()?;
  cr.save()?;
  cr.translate(0.5, 0.5);
  round(cr, 0, 0, WIDTH - 1, HEIGHT - 1, RADIUS - 0.5);
  color(cr, ACCENT, 0.25);
  cr.set_line_width(1.0);
  cr.stroke()?;
  cr.restore()?;
  search_field(cr, view)?;
  if matches!(view.step, Step::Wipe) {
    return wipe(cr);
  }
  if let Status::Failed(error) = &view.status {
    message(cr, error, ERROR);
  } else if view.matches.is_empty() {
    let searching = !view.query.is_empty();
    let text = match (&view.step, view.mode) {
      _ if view.status != Status::Ready => "Loading…",
      (Step::Actions(..), _) => "No matching actions",
      (_, Mode::Apps) if searching => "No matching applications",
      (_, Mode::Apps) => "No applications found",
      (_, Mode::Clipboard) if !picker.clips.is_empty() => "No matching entries",
      (_, Mode::Clipboard) => "Clipboard history is empty",
      (_, Mode::Passwords) if !picker.logins.is_empty() => "No matching entries",
      (_, Mode::Passwords) => "The vault is empty",
    };
    message(cr, text, DIM);
  } else {
    list(cr, picker, view)?;
  }
  Ok(())
}

fn search_field(cr: &Context, view: &View) -> cairo::Result<()> {
  round(cr, PAD, PAD, ROW_WIDTH, FIELD, 10.0);
  color(cr, 0x313244, 0.9);
  cr.fill()?;
  color(cr, DIM, 1.0);
  search_symbol(cr, PAD + 20, PAD + FIELD / 2)?;
  let placeholder = match (&view.step, view.mode) {
    (Step::Actions(login, ..), _) => format!("Choose an action for {}", login.name),
    (_, Mode::Apps) => "Search applications".into(),
    (_, Mode::Clipboard) => "Search clipboard".into(),
    (_, Mode::Passwords) => "Search passwords".into(),
  };
  let empty = view.query.is_empty();
  let layout = text_layout(
    cr,
    if empty { &placeholder } else { &view.query },
    15,
    Weight::Normal,
  );
  let (_, extents) = layout.pixel_extents();
  let x = PAD + 38;
  let width = ROW_WIDTH - 52;
  let caret = if empty { 0 } else { extents.width() };
  let scroll = f64::from((caret - width + 2).max(0));
  let y = f64::from(PAD + (FIELD - extents.height()) / 2);
  cr.save()?;
  cr.rectangle(
    f64::from(x),
    f64::from(PAD),
    f64::from(width),
    f64::from(FIELD),
  );
  cr.clip();
  color(cr, if empty { DIM } else { TEXT }, 1.0);
  cr.move_to(f64::from(x) - scroll, y);
  pangocairo::functions::show_layout(cr, &layout);
  color(cr, ACCENT, 1.0);
  cr.rectangle(
    f64::from(x + caret) - scroll,
    y + 1.0,
    1.5,
    f64::from(extents.height() - 2),
  );
  cr.fill()?;
  cr.restore()
}

fn message(cr: &Context, text: &str, rgb: u32) {
  color(cr, rgb, 1.0);
  label_lines(
    cr,
    text,
    (PAD + 30, LIST_Y + LIST_HEIGHT / 2 - 20, ROW_WIDTH - 60),
    13,
    Weight::Normal,
    Alignment::Center,
    3,
  );
}

fn wipe(cr: &Context) -> cairo::Result<()> {
  color(cr, BRIGHT, 1.0);
  label(
    cr,
    "Wipe all clipboard history?",
    (WIDTH / 2, BUTTON_Y - 56, 0),
    16,
    Weight::Semibold,
    Alignment::Center,
  );
  color(cr, DIM, 1.0);
  label(
    cr,
    "Enter wipes and Escape cancels",
    (WIDTH / 2, BUTTON_Y - 30, 0),
    12,
    Weight::Normal,
    Alignment::Center,
  );
  for (index, (x, name)) in buttons().into_iter().enumerate() {
    let confirm = index == 1;
    round(cr, x, BUTTON_Y, BUTTON.0, BUTTON.1, 8.0);
    if confirm {
      color(cr, ERROR, 0.9);
    } else {
      color(cr, 0x45475a, 0.78);
    }
    cr.fill()?;
    color(cr, if confirm { 0x1e1e2e } else { TEXT }, 1.0);
    label(
      cr,
      name,
      (x, BUTTON_Y + 8, BUTTON.0),
      13,
      Weight::Semibold,
      Alignment::Center,
    );
  }
  Ok(())
}

fn list(cr: &Context, picker: &Picker, view: &View) -> cairo::Result<()> {
  let height = row_height(view);
  let visible = rows(view);
  for (row, &index) in view
    .matches
    .iter()
    .enumerate()
    .skip(view.offset)
    .take(visible)
  {
    let y = LIST_Y + (row - view.offset) as i32 * height;
    let selected = row == view.selected;
    if selected || view.hover == Some(row) {
      round(cr, PAD, y, ROW_WIDTH, height, 10.0);
      if selected {
        color(cr, 0x45475a, 0.7);
      } else {
        color(cr, 0x313244, 0.55);
      }
      cr.fill()?;
    }
    match (&view.step, view.mode) {
      (Step::Actions(..), _) => {
        entry(
          cr,
          vault::ACTIONS[index].1,
          None,
          TEXT_X,
          y,
          height,
          selected,
        );
      }
      (_, Mode::Apps) => {
        let app = &picker.apps[index];
        let icon = app
          .icon
          .as_ref()
          .and_then(|icon| picker.icons.get(icon))
          .and_then(Option::as_ref);
        let top = y + (height - ICON) / 2;
        if let Some(icon) = icon {
          fit(cr, icon, (TEXT_X, top, ICON, ICON), 0.0)?;
        } else {
          let initial = app
            .name
            .chars()
            .next()
            .map(String::from)
            .unwrap_or_default();
          picture(cr, None, TEXT_X, top, ICON, &initial)?;
        }
        let x = TEXT_X + ICON + 12;
        entry(cr, &app.name, app.detail.as_deref(), x, y, height, selected);
      }
      (_, Mode::Clipboard) => {
        let clip = &picker.clips[index];
        if let Some((kind, size)) = &clip.image {
          let frame = (
            TEXT_X,
            y + (height - THUMBNAIL.1) / 2,
            THUMBNAIL.0,
            THUMBNAIL.1,
          );
          match picker.thumbs.get(&clip.id).and_then(Option::as_ref) {
            Some(thumbnail) => fit(cr, thumbnail, frame, 6.0)?,
            None => {
              round(cr, frame.0, frame.1, frame.2, frame.3, 6.0);
              color(cr, 0x313244, 0.8);
              cr.fill()?;
            }
          }
          let x = TEXT_X + THUMBNAIL.0 + 14;
          entry(cr, kind, Some(size), x, y, height, selected);
        } else {
          color(cr, if selected { BRIGHT } else { TEXT }, 1.0);
          label_lines(
            cr,
            &clip.text,
            (TEXT_X, y + 12, ROW_WIDTH - 24),
            13,
            Weight::Normal,
            Alignment::Left,
            3,
          );
        }
      }
      (_, Mode::Passwords) => {
        let login = &picker.logins[index];
        let user = Some(login.user.as_str()).filter(|user| !user.is_empty());
        entry(cr, &login.name, user, TEXT_X, y, height, selected);
      }
    }
  }
  let total = view.matches.len();
  if total > visible {
    let thumb = (LIST_HEIGHT * visible as i32 / total as i32).max(24);
    let top = LIST_Y + (LIST_HEIGHT - thumb) * view.offset as i32 / (total - visible) as i32;
    round(cr, WIDTH - 7, top, 3, thumb, 1.5);
    color(cr, TEXT, 0.25);
    cr.fill()?;
  }
  Ok(())
}

fn entry(
  cr: &Context,
  title: &str,
  detail: Option<&str>,
  x: i32,
  y: i32,
  height: i32,
  selected: bool,
) {
  let width = PAD + ROW_WIDTH - 12 - x;
  let middle = y + height / 2;
  color(cr, if selected { BRIGHT } else { TEXT }, 1.0);
  let top = if detail.is_some() {
    middle - 18
  } else {
    middle - 9
  };
  label(
    cr,
    title,
    (x, top, width),
    14,
    Weight::Normal,
    Alignment::Left,
  );
  if let Some(detail) = detail {
    color(cr, DIM, 1.0);
    label(
      cr,
      detail,
      (x, middle + 1, width),
      11,
      Weight::Normal,
      Alignment::Left,
    );
  }
}

/// Paints an image centered inside a frame without cropping it.
fn fit(
  cr: &Context,
  image: &ImageSurface,
  (x, y, width, height): (i32, i32, i32, i32),
  radius: f64,
) -> cairo::Result<()> {
  let (w, h) = (f64::from(image.width()), f64::from(image.height()));
  let scale = (f64::from(width) / w).min(f64::from(height) / h);
  let (drawn_w, drawn_h) = (w * scale, h * scale);
  let left = f64::from(x) + (f64::from(width) - drawn_w) / 2.0;
  let top = f64::from(y) + (f64::from(height) - drawn_h) / 2.0;
  cr.save()?;
  if radius > 0.0 {
    round(
      cr,
      left.round() as i32,
      top.round() as i32,
      drawn_w.round() as i32,
      drawn_h.round() as i32,
      radius,
    );
    cr.clip();
  }
  cr.translate(left, top);
  cr.scale(scale, scale);
  cr.set_source_surface(image, 0.0, 0.0)?;
  cr.paint()?;
  cr.restore()
}
