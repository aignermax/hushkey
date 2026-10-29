//! The tray image: hushkey's logo with a coloured badge for the state, as
//! the Python app draws it.

/// The logo, 512x512 RGBA PNG.
const LOGO: &[u8] = include_bytes!("../../../../assets/logo.png");

/// Tray icon edge in pixels; Windows scales it to the tray size.
pub const SIZE: u32 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    None,
    Loading,
    Recording,
    Transcribing,
    Listening,
    Failed,
}

impl Badge {
    fn colour(self) -> Option<[u8; 3]> {
        match self {
            Badge::None => None,
            Badge::Loading => Some([0x2b, 0x6c, 0xb0]),
            Badge::Recording => Some([0xe5, 0x3e, 0x3e]),
            Badge::Transcribing => Some([0xdd, 0x6b, 0x20]),
            Badge::Listening => Some([0x38, 0xa1, 0x69]),
            Badge::Failed => Some([0x71, 0x80, 0x96]),
        }
    }
}

/// Decode a PNG into RGBA bytes and its width and height.
pub fn decode_png(bytes: &[u8]) -> (Vec<u8>, u32, u32) {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().expect("a valid PNG");
    let mut buf = vec![0; reader.output_buffer_size().expect("a sized PNG")];
    let info = reader.next_frame(&mut buf).expect("a PNG frame");
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .chunks(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf
            .chunks(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => unreachable!("EXPAND turns palettes into RGB(A)"),
    };
    (rgba, info.width, info.height)
}

/// Shrink a square RGBA image by averaging `factor`x`factor` blocks
/// (alpha-weighted, so transparent pixels do not darken edges).
pub fn shrink(rgba: &[u8], size: u32, factor: u32) -> Vec<u8> {
    let out = size / factor;
    let mut result = Vec::with_capacity((out * out * 4) as usize);
    for oy in 0..out {
        for ox in 0..out {
            let (mut r, mut g, mut b, mut a) = (0u64, 0u64, 0u64, 0u64);
            for y in oy * factor..(oy + 1) * factor {
                for x in ox * factor..(ox + 1) * factor {
                    let i = ((y * size + x) * 4) as usize;
                    let alpha = rgba[i + 3] as u64;
                    r += rgba[i] as u64 * alpha;
                    g += rgba[i + 1] as u64 * alpha;
                    b += rgba[i + 2] as u64 * alpha;
                    a += alpha;
                }
            }
            let n = (factor * factor) as u64;
            // Fully transparent blocks stay transparent black.
            let pixel = match (r.checked_div(a), g.checked_div(a), b.checked_div(a)) {
                (Some(r), Some(g), Some(b)) => [r as u8, g as u8, b as u8, (a / n) as u8],
                _ => [0, 0, 0, 0],
            };
            result.extend_from_slice(&pixel);
        }
    }
    result
}

/// The tray image for `badge`, `SIZE`x`SIZE` RGBA.
pub fn tray(badge: Badge) -> Vec<u8> {
    let (logo, width, _) = decode_png(LOGO);
    let mut image = shrink(&logo, width, width / SIZE);
    if let Some(colour) = badge.colour() {
        // A disc in the lower right corner with a dark ring, like the
        // Python app's badge at (36, 36)..(62, 62) on 64 px.
        let (cx, cy, radius, ring) = (49.0f32, 49.0f32, 13.0f32, 3.0f32);
        for y in 0..SIZE {
            for x in 0..SIZE {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let cover = (radius + 0.5 - d).clamp(0.0, 1.0);
                if cover == 0.0 {
                    continue;
                }
                let fill = if d > radius - ring {
                    [0x2d, 0x35, 0x42]
                } else {
                    colour
                };
                let i = ((y * SIZE + x) * 4) as usize;
                for c in 0..3 {
                    image[i + c] =
                        (fill[c] as f32 * cover + image[i + c] as f32 * (1.0 - cover)) as u8;
                }
                image[i + 3] = (255.0 * cover + image[i + 3] as f32 * (1.0 - cover)) as u8;
            }
        }
    }
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_logo_decodes_and_shrinks_to_the_tray_size() {
        let (logo, w, h) = decode_png(LOGO);
        assert_eq!((w, h), (512, 512));
        assert_eq!(logo.len(), 512 * 512 * 4);
        let image = tray(Badge::None);
        assert_eq!(image.len(), (SIZE * SIZE * 4) as usize);
        assert!(image.chunks(4).any(|p| p[3] > 200), "the logo is visible");
    }

    #[test]
    fn a_badge_colours_the_lower_right_corner() {
        let image = tray(Badge::Recording);
        let i = ((49 * SIZE + 49) * 4) as usize;
        assert_eq!(&image[i..i + 4], &[0xe5, 0x3e, 0x3e, 255]);
        assert_ne!(tray(Badge::None), image);
    }

    #[test]
    fn shrinking_averages_and_respects_transparency() {
        // 2x2 -> 1x1: one opaque white, three transparent pixels.
        let rgba = [255, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(shrink(&rgba, 2, 2), [255, 255, 255, 63]);
    }
}

#[cfg(test)]
mod preview {
    use super::*;

    /// Writes every badge side by side to HUSHKEY_ICON_SHOT, for a look.
    #[test]
    #[ignore = "writes an image for a person to look at"]
    fn render_all_badges() {
        let Some(path) = std::env::var_os("HUSHKEY_ICON_SHOT") else {
            return;
        };
        let badges = [
            Badge::None,
            Badge::Loading,
            Badge::Recording,
            Badge::Transcribing,
            Badge::Listening,
            Badge::Failed,
        ];
        let n = badges.len() as u32;
        let mut strip = vec![255u8; (SIZE * n * SIZE * 4) as usize];
        for (i, badge) in badges.iter().enumerate() {
            let image = tray(*badge);
            for y in 0..SIZE {
                for x in 0..SIZE {
                    let src = ((y * SIZE + x) * 4) as usize;
                    let dst = ((y * SIZE * n + i as u32 * SIZE + x) * 4) as usize;
                    let a = image[src + 3] as u32;
                    for c in 0..3 {
                        strip[dst + c] =
                            ((image[src + c] as u32 * a + 255 * (255 - a)) / 255) as u8;
                    }
                }
            }
        }
        let mut png = png::Encoder::new(std::fs::File::create(path).unwrap(), SIZE * n, SIZE);
        png.set_color(png::ColorType::Rgba);
        png.set_depth(png::BitDepth::Eight);
        png.write_header()
            .unwrap()
            .write_image_data(&strip)
            .unwrap();
    }
}
