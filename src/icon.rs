//! Draws the tray icon: a green disc with a checkmark when everything is in
//! sync, or a red disc showing how many repos aren't.

pub const SIZE: u32 = 32;
const SUPERSAMPLE: u32 = 4;
const GREEN: [u8; 3] = [46, 160, 67];
const RED: [u8; 3] = [215, 58, 73];
const GRAY: [u8; 3] = [128, 128, 128];

/// What the icon should show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    /// No result yet.
    Checking,
    AllSynced,
    Unsynced(usize),
    /// Bad config or a missing folder.
    Problem,
}

/// 5×7 glyphs, one row per byte, high bit on the left.
fn glyph(c: char) -> [u8; 7] {
    match c {
        '0' => [0x70, 0x88, 0x98, 0xA8, 0xC8, 0x88, 0x70],
        '1' => [0x20, 0x60, 0x20, 0x20, 0x20, 0x20, 0x70],
        '2' => [0x70, 0x88, 0x08, 0x10, 0x20, 0x40, 0xF8],
        '3' => [0xF8, 0x10, 0x20, 0x10, 0x08, 0x88, 0x70],
        '4' => [0x10, 0x30, 0x50, 0x90, 0xF8, 0x10, 0x10],
        '5' => [0xF8, 0x80, 0xF0, 0x08, 0x08, 0x88, 0x70],
        '6' => [0x30, 0x40, 0x80, 0xF0, 0x88, 0x88, 0x70],
        '7' => [0xF8, 0x08, 0x10, 0x20, 0x40, 0x40, 0x40],
        '8' => [0x70, 0x88, 0x88, 0x70, 0x88, 0x88, 0x70],
        '9' => [0x70, 0x88, 0x88, 0x78, 0x08, 0x10, 0x60],
        '+' => [0x00, 0x20, 0x20, 0xF8, 0x20, 0x20, 0x00],
        '!' => [0x20, 0x20, 0x20, 0x20, 0x20, 0x00, 0x20],
        '…' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xA8],
        _ => [0; 7],
    }
}

/// Returns `SIZE`×`SIZE` straight (non-premultiplied) RGBA pixels.
pub fn render(badge: Badge) -> Vec<u8> {
    let n = SIZE * SUPERSAMPLE;
    // Coverage masks at supersampled resolution: disc and white foreground.
    let mut disc = vec![false; (n * n) as usize];
    let mut fg = vec![false; (n * n) as usize];
    let center = n as f32 / 2.0;
    let radius = center - SUPERSAMPLE as f32 * 0.5;
    for y in 0..n {
        for x in 0..n {
            let (dx, dy) = (x as f32 + 0.5 - center, y as f32 + 0.5 - center);
            disc[(y * n + x) as usize] = dx * dx + dy * dy <= radius * radius;
        }
    }

    let color = match badge {
        Badge::Checking => {
            draw_text(&mut fg, n, "…");
            GRAY
        }
        Badge::AllSynced => {
            let s = n as f32 / 32.0;
            let points = [
                (9.0 * s, 16.5 * s),
                (14.0 * s, 22.0 * s),
                (23.5 * s, 11.0 * s),
            ];
            draw_polyline(&mut fg, n, &points, 3.6 * s);
            GREEN
        }
        Badge::Problem => {
            draw_text(&mut fg, n, "!");
            RED
        }
        Badge::Unsynced(count) => {
            let text = if count > 99 {
                "99+".to_string()
            } else {
                count.to_string()
            };
            draw_text(&mut fg, n, &text);
            RED
        }
    };

    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    let samples = (SUPERSAMPLE * SUPERSAMPLE) as f32;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (mut disc_hits, mut fg_hits) = (0.0, 0.0);
            for sy in 0..SUPERSAMPLE {
                for sx in 0..SUPERSAMPLE {
                    let i = ((y * SUPERSAMPLE + sy) * n + x * SUPERSAMPLE + sx) as usize;
                    if disc[i] {
                        disc_hits += 1.0;
                        if fg[i] {
                            fg_hits += 1.0;
                        }
                    }
                }
            }
            let alpha = disc_hits / samples;
            let white = if disc_hits > 0.0 {
                fg_hits / disc_hits
            } else {
                0.0
            };
            for c in color {
                rgba.push((c as f32 + (255.0 - c as f32) * white).round() as u8);
            }
            rgba.push((alpha * 255.0).round() as u8);
        }
    }
    rgba
}

/// Draws `text` centered, as large as fits inside the disc.
fn draw_text(mask: &mut [bool], n: u32, text: &str) {
    let chars: Vec<char> = text.chars().collect();
    let cols = chars.len() as u32 * 6 - 1; // 5 wide plus 1 gap
    let max_width = n * 13 / 16;
    let max_height = n * 9 / 16;
    let scale = (max_width / cols).min(max_height / 7).max(1);
    let (width, height) = (cols * scale, 7 * scale);
    let (left, top) = ((n - width) / 2, (n - height) / 2);
    for (i, c) in chars.iter().enumerate() {
        let rows = glyph(*c);
        for (row, bits) in rows.iter().enumerate() {
            for col in 0..5 {
                if bits & (0x80 >> col) == 0 {
                    continue;
                }
                let x0 = left + (i as u32 * 6 + col) * scale;
                let y0 = top + row as u32 * scale;
                for y in y0..y0 + scale {
                    for x in x0..x0 + scale {
                        mask[(y * n + x) as usize] = true;
                    }
                }
            }
        }
    }
}

fn draw_polyline(mask: &mut [bool], n: u32, points: &[(f32, f32)], width: f32) {
    let r = width / 2.0;
    for y in 0..n {
        for x in 0..n {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            if points
                .windows(2)
                .any(|seg| distance_to_segment(p, seg[0], seg[1]) <= r)
            {
                mask[(y * n + x) as usize] = true;
            }
        }
    }
}

fn distance_to_segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / (abx * abx + aby * aby)).clamp(0.0, 1.0);
    let (cx, cy) = (a.0 + t * abx, a.1 + t * aby);
    ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_every_badge() {
        for badge in [
            Badge::Checking,
            Badge::AllSynced,
            Badge::Unsynced(3),
            Badge::Unsynced(42),
            Badge::Unsynced(150),
            Badge::Problem,
        ] {
            let px = render(badge);
            assert_eq!(px.len(), (SIZE * SIZE * 4) as usize);
            // Corners are transparent, the center is opaque.
            assert_eq!(px[3], 0);
            let mid = ((SIZE / 2 * SIZE + SIZE / 2) * 4 + 3) as usize;
            assert_eq!(px[mid], 255);
        }
    }
}
