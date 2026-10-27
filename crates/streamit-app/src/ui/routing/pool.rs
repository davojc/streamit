//! The media pool's pictures on the Stream ATEM and Screens ATEM sub-tabs: each used slot's
//! thumbnail (downloaded from the switcher one at a time while the page is open, and kept
//! for the session), and putting a picture in a slot, saving one to a file and clearing a
//! slot. Nothing is downloaded or changed while a service is under way.
use super::super::{Pending, StreamItApp};
use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use streamit_core::devices::{Still, Switcher};
use streamit_core::golive::Stage;
use streamit_core::routing::Target;
use tracing::{info, warn};

/// A thumbnail's size in pixels (16:9).
const THUMB: (u32, u32) = (240, 135);
/// A still's name on the switcher holds at most this many bytes.
const NAME_MAX: usize = 63;

type Fetched = (Target, u16, Option<String>, Result<ColorImage, String>);
/// A slot's thumbnail: the still name it was made for, and the picture (or why there is none).
type Made = (Option<String>, Result<TextureHandle, String>);

#[derive(Default)]
pub(in crate::ui) struct Pool {
    /// By (switcher, slot from 0): the name the thumbnail was made for, and the picture (or
    /// why there is none).
    thumbs: HashMap<(Target, u16), Made>,
    fetch: Pending<Fetched>,
    /// A picture being put in, saved or cleared; what to say when it is done.
    action: Pending<(Target, u16, Result<String, String>)>,
    /// The slot that action is on, and what it is doing ("Putting in…").
    busy: Option<(Target, u16, &'static str)>,
}

/// What a slot shows for its picture.
pub(in crate::ui) enum Thumb<'a> {
    Picture(&'a TextureHandle),
    Working(&'static str),
    Failed(&'a str),
    Waiting,
    Empty,
}

/// A service is under way: the pool is left alone (no lock, no change to a still on air).
pub(in crate::ui) fn under_way(stage: &Stage) -> bool {
    matches!(stage, Stage::Prepared(_) | Stage::Countdown { .. } | Stage::Live | Stage::Ending { .. })
}

/// `picture` scaled to fit `w`x`h` without stretching, centred on black (opaque).
pub(in crate::ui) fn fit(picture: &image::RgbaImage, w: u32, h: u32) -> Still {
    let (pw, ph) = picture.dimensions();
    let scale = (f64::from(w) / f64::from(pw.max(1))).min(f64::from(h) / f64::from(ph.max(1)));
    let (sw, sh) =
        (((f64::from(pw) * scale).round() as u32).clamp(1, w), ((f64::from(ph) * scale).round() as u32).clamp(1, h));
    let scaled = image::imageops::resize(picture, sw, sh, image::imageops::FilterType::Triangle);
    let mut canvas = image::RgbaImage::from_pixel(w, h, image::Rgba([0, 0, 0, 255]));
    image::imageops::overlay(&mut canvas, &scaled, i64::from((w - sw) / 2), i64::from((h - sh) / 2));
    // Transparent parts show black on air.
    for p in canvas.pixels_mut() {
        let a = u16::from(p[3]);
        for c in 0..3 {
            p[c] = (u16::from(p[c]) * a / 255) as u8;
        }
        p[3] = 255;
    }
    Still { width: w, height: h, rgba: canvas.into_raw() }
}

/// The name a picture file gets on the switcher: its file name without the extension.
pub(in crate::ui) fn still_name(path: &std::path::Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut name = String::new();
    for ch in stem.trim().chars() {
        if name.len() + ch.len_utf8() > NAME_MAX {
            break;
        }
        name.push(ch);
    }
    if name.is_empty() { "Picture".into() } else { name }
}

fn image_of(still: Still) -> Option<image::RgbaImage> {
    image::RgbaImage::from_raw(still.width, still.height, still.rgba)
}

fn thumbnail(still: Still) -> Result<ColorImage, String> {
    let img = image_of(still).ok_or("the switcher sent a still of the wrong size")?;
    let t = image::imageops::thumbnail(&img, THUMB.0, THUMB.1);
    Ok(ColorImage::from_rgba_unmultiplied([t.width() as usize, t.height() as usize], t.as_raw()))
}

impl StreamItApp {
    fn pool_switcher(&self, target: Target) -> Option<Arc<dyn Switcher>> {
        let d = self.devices.borrow().clone()?;
        match target {
            Target::StreamAtem => Some(d.stream as Arc<dyn Switcher>),
            Target::ScreensAtem => Some(d.screens),
            _ => None,
        }
    }

    /// Picks up finished downloads and actions, and starts the next thumbnail: the first used
    /// slot whose thumbnail is missing or was made for another name.
    pub(in crate::ui) fn pool_tick(
        &mut self,
        ctx: &egui::Context,
        target: Target,
        stills: &[Option<String>],
        stage: &Stage,
    ) {
        if let Some((t, slot, name, got)) = self.pool.fetch.take() {
            let tex =
                got.map(|img| ctx.load_texture(format!("pool-{}-{slot}", t.index()), img, TextureOptions::LINEAR));
            if let Err(e) = &tex {
                warn!(target = t.name(), slot, "media pool thumbnail: {e}");
            }
            self.pool.thumbs.insert((t, slot), (name, tex));
        }
        if let Some((t, slot, done)) = self.pool.action.take() {
            self.pool.busy = None;
            // Read it again: it may hold a new picture.
            self.pool.thumbs.remove(&(t, slot));
            match done {
                Ok(m) if m.is_empty() => {}
                Ok(m) | Err(m) => self.notice = Some(m),
            }
        }
        if self.pool.fetch.running || self.pool.action.running || under_way(stage) {
            return;
        }
        let next = stills.iter().enumerate().find_map(|(i, name)| {
            let slot = i as u16;
            let name = name.as_ref()?;
            let stale = self.pool.thumbs.get(&(target, slot)).is_none_or(|(had, _)| had.as_ref() != Some(name));
            stale.then(|| (slot, name.clone()))
        });
        let (Some((slot, name)), Some(sw)) = (next, self.pool_switcher(target)) else { return };
        self.pool.fetch.start(&self.rt, ctx, async move {
            let got = match sw.download_still(slot).await {
                Ok(still) => {
                    tokio::task::spawn_blocking(move || thumbnail(still)).await.unwrap_or_else(|e| Err(e.to_string()))
                }
                Err(e) => Err(e.to_string()),
            };
            (target, slot, Some(name), got)
        });
    }

    /// What slot `slot` shows for its picture; `name` is its still's name now.
    pub(in crate::ui) fn pool_thumb(&self, target: Target, slot: u16, name: Option<&String>) -> Thumb<'_> {
        if let Some((t, s, what)) = self.pool.busy
            && (t, s) == (target, slot)
        {
            return Thumb::Working(what);
        }
        if name.is_none() {
            return Thumb::Empty;
        }
        match self.pool.thumbs.get(&(target, slot)) {
            Some((had, Ok(tex))) if had.as_ref() == name => Thumb::Picture(tex),
            Some((had, Err(e))) if had.as_ref() == name => Thumb::Failed(e),
            _ => Thumb::Waiting,
        }
    }

    /// Something is being put in, saved or cleared (one at a time).
    pub(in crate::ui) fn pool_busy(&self) -> bool {
        self.pool.action.running
    }

    /// Asks for a picture file, fits it to the switcher's format and puts it in `slot`.
    pub(in crate::ui) fn put_picture(&mut self, target: Target, slot: u16) {
        let Some(sw) = self.pool_switcher(target) else { return };
        self.pool.busy = Some((target, slot, "Putting in…"));
        self.pool.action.start(&self.rt, &self.ctx, async move {
            let Some(file) = rfd::AsyncFileDialog::new()
                .add_filter("Picture", &["png", "jpg", "jpeg"])
                .pick_file()
                .await
                .map(|f| f.path().to_path_buf())
            else {
                return (target, slot, Ok(String::new()));
            };
            let done = async {
                let (w, h) = sw.still_size().ok_or("the switcher has not said its video format")?;
                let name = still_name(&file);
                let path = file.clone();
                let still = tokio::task::spawn_blocking(move || {
                    let img = image::open(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                    Ok::<_, String>(fit(&img.to_rgba8(), w, h))
                })
                .await
                .map_err(|e| e.to_string())??;
                info!(target = target.name(), slot, name = %name, "putting a picture in the media pool");
                sw.upload_still(slot, still, &name).await.map_err(|e| e.to_string())?;
                Ok::<_, String>(format!("Media slot {} now holds “{name}”.", slot + 1))
            };
            (target, slot, done.await.map_err(|e| format!("Could not put the picture in slot {}: {e}", slot + 1)))
        });
    }

    /// Asks where to save, then downloads the still in `slot` and writes it as a PNG.
    pub(in crate::ui) fn save_still(&mut self, target: Target, slot: u16, name: String) {
        let Some(sw) = self.pool_switcher(target) else { return };
        self.pool.busy = Some((target, slot, "Saving…"));
        self.pool.action.start(&self.rt, &self.ctx, async move {
            let Some(file): Option<PathBuf> = rfd::AsyncFileDialog::new()
                .add_filter("PNG picture", &["png"])
                .set_file_name(format!("{name}.png"))
                .save_file()
                .await
                .map(|f| f.path().to_path_buf())
            else {
                return (target, slot, Ok(String::new()));
            };
            let done = async {
                let still = sw.download_still(slot).await.map_err(|e| e.to_string())?;
                let path = file.clone();
                tokio::task::spawn_blocking(move || {
                    let img = image_of(still).ok_or("the switcher sent a still of the wrong size")?;
                    img.save_with_format(&path, image::ImageFormat::Png).map_err(|e| e.to_string())
                })
                .await
                .map_err(|e| e.to_string())??;
                Ok::<_, String>(format!("Saved media slot {} to {}.", slot + 1, file.display()))
            };
            (target, slot, done.await.map_err(|e| format!("Could not save slot {}: {e}", slot + 1)))
        });
    }

    pub(in crate::ui) fn clear_slot(&mut self, target: Target, slot: u16) {
        let Some(sw) = self.pool_switcher(target) else { return };
        info!(target = target.name(), slot, "clearing a media pool slot");
        self.pool.busy = Some((target, slot, "Clearing…"));
        self.pool.action.start(&self.rt, &self.ctx, async move {
            let done = sw.clear_still(slot).await.map(|()| format!("Media slot {} is empty now.", slot + 1));
            (target, slot, done.map_err(|e| format!("Could not clear slot {}: {e}", slot + 1)))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_is_fitted_without_stretching_on_black() {
        // A 4:3 white picture into 16:9: black bars left and right.
        let white = image::RgbaImage::from_pixel(400, 300, image::Rgba([255, 255, 255, 255]));
        let s = fit(&white, 160, 90);
        assert_eq!((s.width, s.height, s.rgba.len()), (160, 90, 160 * 90 * 4));
        let at = |x: u32, y: u32| &s.rgba[((y * 160 + x) * 4) as usize..((y * 160 + x) * 4 + 4) as usize];
        assert_eq!(at(2, 45), [0, 0, 0, 255], "bar");
        assert_eq!(at(80, 45), [255, 255, 255, 255], "picture");
        // Transparent pixels come out black and opaque.
        let clear = image::RgbaImage::from_pixel(16, 9, image::Rgba([255, 0, 0, 0]));
        assert_eq!(&fit(&clear, 16, 9).rgba[..4], [0, 0, 0, 255]);
    }

    #[test]
    fn a_still_is_named_after_its_file() {
        assert_eq!(still_name(std::path::Path::new(r"C:\Pics\Notices – Autumn.png")), "Notices – Autumn");
        assert_eq!(still_name(std::path::Path::new("")), "Picture");
        let long = "a".repeat(100) + ".jpg";
        assert_eq!(still_name(std::path::Path::new(&long)).len(), NAME_MAX);
    }

    #[test]
    fn the_pool_is_left_alone_during_a_service() {
        assert!(!under_way(&Stage::Idle));
        assert!(!under_way(&Stage::Ended));
        assert!(under_way(&Stage::Live));
    }
}
