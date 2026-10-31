//! Draws a `KeyLook` as a 96x96 key image (the XL key size). The elgato-streamdeck
//! crate converts it to the JPEG (with the XL's mirroring) on upload.
use crate::layout::{KeyLook, Rgb};
use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use image::{DynamicImage, Rgb as ImgRgb, RgbImage};

pub const KEY_PX: u32 = 96;
/// Atkinson Hyperlegible Bold, SIL OFL 1.1 (see `assets/OFL.txt`).
static FONT_BYTES: &[u8] = include_bytes!("../assets/AtkinsonHyperlegible-Bold.ttf");
const MARGIN: f32 = 4.0;

fn font() -> FontRef<'static> {
    FontRef::try_from_slice(FONT_BYTES).expect("bundled font is valid")
}

fn line_width<F: Font, S: ScaleFont<F>>(f: &S, line: &str) -> f32 {
    let mut w = 0.0;
    let mut prev = None;
    for c in line.chars() {
        let id = f.glyph_id(c);
        if let Some(p) = prev {
            w += f.kern(p, id);
        }
        w += f.h_advance(id);
        prev = Some(id);
    }
    w
}

/// Largest font size (max 30 px) at which every line fits inside the key.
fn fit_scale(font: &FontRef<'_>, lines: &[&str]) -> PxScale {
    let avail = KEY_PX as f32 - 2.0 * MARGIN;
    let mut px = 30.0_f32;
    while px > 9.0 {
        let s = font.as_scaled(PxScale::from(px));
        let widest = lines.iter().map(|l| line_width(&s, l)).fold(0.0, f32::max);
        let height = s.height() * lines.len() as f32;
        if widest <= avail && height <= avail {
            break;
        }
        px -= 1.0;
    }
    PxScale::from(px)
}

fn px(c: Rgb) -> ImgRgb<u8> {
    ImgRgb([c.0, c.1, c.2])
}

/// How wide a key's frame is, in pixels.
const BORDER_PX: u32 = 5;

pub fn render_key(look: &KeyLook) -> DynamicImage {
    let mut img = RgbImage::from_pixel(KEY_PX, KEY_PX, px(look.bg));
    if let Some(b) = look.border {
        for (x, y, p) in img.enumerate_pixels_mut() {
            if x < BORDER_PX || y < BORDER_PX || x >= KEY_PX - BORDER_PX || y >= KEY_PX - BORDER_PX {
                *p = px(b);
            }
        }
    }
    let lines: Vec<&str> = look.text.lines().collect();
    if !lines.is_empty() {
        let font = font();
        let scale = fit_scale(&font, &lines);
        let s = font.as_scaled(scale);
        let block = s.height() * lines.len() as f32;
        let mut top = (KEY_PX as f32 - block) / 2.0;
        for line in &lines {
            let mut x = (KEY_PX as f32 - line_width(&s, line)) / 2.0;
            let baseline = top + s.ascent();
            let mut prev = None;
            for c in line.chars() {
                let id = s.glyph_id(c);
                if let Some(p) = prev {
                    x += s.kern(p, id);
                }
                let glyph = id.with_scale_and_position(scale, ab_glyph::point(x, baseline));
                x += s.h_advance(id);
                prev = Some(id);
                if let Some(o) = font.outline_glyph(glyph) {
                    let b = o.px_bounds();
                    o.draw(|gx, gy, cov| {
                        let (ix, iy) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
                        if ix < 0 || iy < 0 || ix >= KEY_PX as i32 || iy >= KEY_PX as i32 {
                            return;
                        }
                        let p = img.get_pixel_mut(ix as u32, iy as u32);
                        let fg = [look.fg.0, look.fg.1, look.fg.2];
                        for (dst, src) in p.0.iter_mut().zip(fg) {
                            *dst = (f32::from(src) * cov + f32::from(*dst) * (1.0 - cov)).round() as u8;
                        }
                    });
                }
            }
            top += s.height();
        }
    }
    DynamicImage::ImageRgb8(img)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{BLUE, WHITE};

    fn look(text: &str) -> KeyLook {
        KeyLook { text: text.into(), bg: BLUE, fg: WHITE, blink: false, border: None }
    }

    #[test]
    fn key_image_is_96_square_with_background_and_text() {
        let img = render_key(&look("LIVE")).to_rgb8();
        assert_eq!(img.dimensions(), (96, 96));
        assert_eq!(img.get_pixel(0, 0).0, [BLUE.0, BLUE.1, BLUE.2]);
        assert!(img.pixels().any(|p| p.0[0] > 200 && p.0[2] > 200), "white text pixels exist");
    }

    #[test]
    fn a_border_frames_the_key() {
        let mut l = look("");
        l.border = Some(WHITE);
        let img = render_key(&l).to_rgb8();
        assert_eq!(img.get_pixel(0, 0).0, [255, 255, 255]);
        assert_eq!(img.get_pixel(48, 48).0, [BLUE.0, BLUE.1, BLUE.2]);
    }

    #[test]
    fn empty_text_is_plain_background() {
        let img = render_key(&look("")).to_rgb8();
        assert!(img.pixels().all(|p| p.0 == [BLUE.0, BLUE.1, BLUE.2]));
    }

    #[test]
    fn long_multiline_text_stays_inside_margins() {
        let img = render_key(&look("WEB\nPRESENTER")).to_rgb8();
        let bg = [BLUE.0, BLUE.1, BLUE.2];
        for x in [0u32, 1, 2, 93, 94, 95] {
            for y in 0..96 {
                assert_eq!(img.get_pixel(x, y).0, bg, "edge pixel {x},{y} untouched");
            }
        }
    }
}
