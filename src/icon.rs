//! Draws the tray icon: a green disc with a checkmark when everything is in
//! sync, or a red disc showing how many repos aren't. Also draws the folder
//! icons Explorer shows on each repo, and packs them into .ico files.

use crate::git::RepoStatus;
use serde::{Deserialize, Serialize};

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

/// Bump whenever the folder artwork changes: Explorer caches icons by path,
/// so new pixels need a new file name.
pub const ICON_VERSION: u32 = 1;
/// The sizes Explorer picks from, from details view (16) to extra large (256).
pub const FOLDER_SIZES: [u32; 6] = [16, 20, 24, 32, 48, 256];

const BLUE: [u8; 3] = [9, 105, 218];
const ORANGE: [u8; 3] = [219, 109, 40];
const AMBER: [u8; 3] = [204, 136, 0];
const DARK_RED: [u8; 3] = [176, 32, 44];
const WHITE: [u8; 3] = [255, 255, 255];
const FOLDER_BACK: [u8; 3] = [222, 166, 50];
const FOLDER_FRONT: [u8; 3] = [248, 202, 86];
const FOLDER_EDGE: [u8; 3] = [178, 124, 20];

/// One repo folder's status as its Explorer icon shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FolderState {
    Synced,
    Ahead,
    Behind,
    Diverged,
    Dirty,
    NoUpstream,
    Error,
}

impl FolderState {
    pub const ALL: [FolderState; 7] = [
        FolderState::Synced,
        FolderState::Ahead,
        FolderState::Behind,
        FolderState::Diverged,
        FolderState::Dirty,
        FolderState::NoUpstream,
        FolderState::Error,
    ];

    /// The most urgent thing about the repo: an error, then anything that
    /// needs a pull, then a push, then a commit.
    pub fn of(s: &RepoStatus) -> Self {
        if s.error.is_some() {
            Self::Error
        } else if s.is_diverged() {
            Self::Diverged
        } else if s.behind > 0 {
            Self::Behind
        } else if s.ahead > 0 {
            Self::Ahead
        } else if s.dirty {
            Self::Dirty
        } else if !s.has_upstream {
            Self::NoUpstream
        } else {
            Self::Synced
        }
    }

    /// Stable name for file names.
    pub fn name(self) -> &'static str {
        match self {
            Self::Synced => "synced",
            Self::Ahead => "ahead",
            Self::Behind => "behind",
            Self::Diverged => "diverged",
            Self::Dirty => "dirty",
            Self::NoUpstream => "no-upstream",
            Self::Error => "error",
        }
    }

    fn color(self) -> [u8; 3] {
        match self {
            Self::Synced => GREEN,
            Self::Ahead => BLUE,
            Self::Behind => ORANGE,
            Self::Diverged => RED,
            Self::Dirty => AMBER,
            Self::NoUpstream => GRAY,
            Self::Error => DARK_RED,
        }
    }

    /// The white mark inside the disc: strokes and dots in disc units
    /// (-1..1 across, y down).
    fn mark(self) -> Mark {
        match self {
            Self::Synced => Mark::strokes(&[&[(-0.5, 0.02), (-0.14, 0.38), (0.5, -0.32)]]),
            Self::Ahead => Mark::strokes(&[
                &[(0.0, 0.52), (0.0, -0.45)],
                &[(-0.42, -0.06), (0.0, -0.48), (0.42, -0.06)],
            ]),
            Self::Behind => Mark::strokes(&[
                &[(0.0, -0.52), (0.0, 0.45)],
                &[(-0.42, 0.06), (0.0, 0.48), (0.42, 0.06)],
            ]),
            Self::Diverged => Mark::strokes(&[
                &[(-0.27, 0.5), (-0.27, -0.45)],
                &[(-0.54, -0.16), (-0.27, -0.48), (0.0, -0.16)],
                &[(0.27, -0.5), (0.27, 0.45)],
                &[(0.0, 0.16), (0.27, 0.48), (0.54, 0.16)],
            ]),
            Self::Dirty => Mark {
                strokes: Vec::new(),
                dots: vec![((0.0, 0.0), 0.36)],
            },
            Self::NoUpstream => Mark::strokes(&[&[(-0.45, 0.0), (0.45, 0.0)]]),
            Self::Error => Mark {
                strokes: vec![vec![(0.0, -0.5), (0.0, 0.12)]],
                dots: vec![((0.0, 0.46), 0.15)],
            },
        }
    }
}

struct Mark {
    strokes: Vec<Vec<(f32, f32)>>,
    /// Centre and radius.
    dots: Vec<((f32, f32), f32)>,
}

impl Mark {
    const WIDTH: f32 = 0.3;

    fn strokes(lines: &[&[(f32, f32)]]) -> Self {
        Self {
            strokes: lines.iter().map(|l| l.to_vec()).collect(),
            dots: Vec::new(),
        }
    }

    fn covers(&self, p: (f32, f32)) -> bool {
        let r = Self::WIDTH / 2.0;
        self.strokes.iter().any(|line| {
            line.windows(2)
                .any(|seg| distance_to_segment(p, seg[0], seg[1]) <= r)
        }) || self
            .dots
            .iter()
            .any(|&(c, radius)| (p.0 - c.0).powi(2) + (p.1 - c.1).powi(2) <= radius * radius)
    }
}

/// `size`×`size` straight RGBA pixels: a folder with a coloured status disc
/// in the bottom-right corner, white-ringed so it reads on the folder.
pub fn folder_rgba(state: FolderState, size: u32) -> Vec<u8> {
    // The 256 px image has detail to spare; 4×4 would be 1M samples.
    let ss = if size >= 128 { 2 } else { SUPERSAMPLE };
    let n = (size * ss) as f32;
    let (color, mark) = (state.color(), state.mark());
    let samples = (ss * ss) as f32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (mut sum, mut hits) = ([0.0f32; 3], 0.0f32);
            for sy in 0..ss {
                for sx in 0..ss {
                    let p = (
                        ((x * ss + sx) as f32 + 0.5) / n,
                        ((y * ss + sy) as f32 + 0.5) / n,
                    );
                    if let Some(c) = folder_sample(p, color, &mark) {
                        hits += 1.0;
                        for (s, c) in sum.iter_mut().zip(c) {
                            *s += c as f32;
                        }
                    }
                }
            }
            for s in sum {
                rgba.push(if hits > 0.0 {
                    (s / hits).round() as u8
                } else {
                    0
                });
            }
            rgba.push((hits / samples * 255.0).round() as u8);
        }
    }
    rgba
}

/// The colour at `p` (unit square, y down), or `None` where it's transparent.
fn folder_sample(p: (f32, f32), color: [u8; 3], mark: &Mark) -> Option<[u8; 3]> {
    const DISC: (f32, f32) = (0.69, 0.69);
    const DISC_R: f32 = 0.25;
    const RING_R: f32 = 0.3;
    const EDGE: f32 = 0.03;

    let (dx, dy) = (p.0 - DISC.0, p.1 - DISC.1);
    let d2 = dx * dx + dy * dy;
    if d2 <= DISC_R * DISC_R {
        let local = (dx / DISC_R, dy / DISC_R);
        return Some(if mark.covers(local) { WHITE } else { color });
    }
    if d2 <= RING_R * RING_R {
        return Some(WHITE);
    }
    let front = rounded_rect(p, (0.06, 0.34, 0.94, 0.86), 0.06);
    if front <= 0.0 {
        return Some(if front > -EDGE {
            FOLDER_EDGE
        } else {
            FOLDER_FRONT
        });
    }
    let back = rounded_rect(p, (0.06, 0.2, 0.94, 0.86), 0.06).min(rounded_rect(
        p,
        (0.06, 0.13, 0.44, 0.28),
        0.05,
    ));
    if back <= 0.0 {
        return Some(if back > -EDGE {
            FOLDER_EDGE
        } else {
            FOLDER_BACK
        });
    }
    None
}

/// Signed distance from `p` to a rounded rectangle (left, top, right, bottom).
fn rounded_rect(p: (f32, f32), r: (f32, f32, f32, f32), radius: f32) -> f32 {
    let (cx, cy) = ((r.0 + r.2) / 2.0, (r.1 + r.3) / 2.0);
    let (hx, hy) = ((r.2 - r.0) / 2.0 - radius, (r.3 - r.1) / 2.0 - radius);
    let (qx, qy) = ((p.0 - cx).abs() - hx, (p.1 - cy).abs() - hy);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - radius
}

/// Packs `(size, straight RGBA)` images into an .ico: 32 bpp BGRA DIBs
/// (double height, bottom-up rows) with an all-zero AND mask, since the
/// alpha channel already says what's transparent.
pub fn encode_ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    const DIR: usize = 6;
    const ENTRY: usize = 16;
    const HEADER: u32 = 40;
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // icon
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());

    let mut offset = (DIR + ENTRY * images.len()) as u32;
    let mut bodies = Vec::new();
    for (size, rgba) in images {
        let size = *size;
        let mask_row = size.div_ceil(32) * 4;
        let pixels = size * size * 4;
        let len = HEADER + pixels + mask_row * size;
        // A 256 px entry is written as 0: the field is one byte.
        let dim = if size >= 256 { 0 } else { size as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += len;

        bodies.extend_from_slice(&HEADER.to_le_bytes());
        bodies.extend_from_slice(&(size as i32).to_le_bytes());
        bodies.extend_from_slice(&(size as i32 * 2).to_le_bytes());
        bodies.extend_from_slice(&1u16.to_le_bytes());
        bodies.extend_from_slice(&32u16.to_le_bytes());
        bodies.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
        bodies.extend_from_slice(&(pixels + mask_row * size).to_le_bytes());
        bodies.extend_from_slice(&[0; 16]); // resolution and palette counts
        for y in (0..size).rev() {
            for x in 0..size {
                let i = ((y * size + x) * 4) as usize;
                let px = rgba.get(i..i + 4).unwrap_or(&[0; 4]);
                bodies.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
        }
        bodies.resize(bodies.len() + (mask_row * size) as usize, 0);
    }
    out.extend_from_slice(&bodies);
    out
}

/// The complete .ico for one state, every size in [`FOLDER_SIZES`].
pub fn folder_ico(state: FolderState) -> Vec<u8> {
    let images: Vec<(u32, Vec<u8>)> = FOLDER_SIZES
        .iter()
        .map(|&size| (size, folder_rgba(state, size)))
        .collect();
    encode_ico(&images)
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

    fn status(ahead: u32, behind: u32, dirty: bool, upstream: bool, error: bool) -> RepoStatus {
        RepoStatus {
            ahead,
            behind,
            dirty,
            has_upstream: upstream,
            error: error.then(|| "boom".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn folder_state_priority() {
        use FolderState::*;
        assert_eq!(FolderState::of(&status(0, 0, false, true, false)), Synced);
        assert_eq!(
            FolderState::of(&status(0, 0, false, false, false)),
            NoUpstream
        );
        assert_eq!(FolderState::of(&status(0, 0, true, false, false)), Dirty);
        assert_eq!(FolderState::of(&status(2, 0, true, true, false)), Ahead);
        assert_eq!(FolderState::of(&status(2, 1, true, true, false)), Diverged);
        assert_eq!(FolderState::of(&status(0, 1, true, true, false)), Behind);
        assert_eq!(FolderState::of(&status(2, 1, true, true, true)), Error);
    }

    #[test]
    fn folder_icons_have_transparent_corners() {
        for state in FolderState::ALL {
            for size in FOLDER_SIZES {
                let px = folder_rgba(state, size);
                assert_eq!(px.len(), (size * size * 4) as usize);
                let alpha = |x: u32, y: u32| px[((y * size + x) * 4 + 3) as usize];
                let last = size - 1;
                for (x, y) in [(0, 0), (last, 0), (0, last), (last, last)] {
                    assert_eq!(alpha(x, y), 0, "{state:?} {size} corner {x},{y}");
                }
                // The folder body and the disc are opaque.
                assert_eq!(alpha(size / 4, size / 2), 255, "{state:?} {size} body");
                assert_eq!(alpha(size * 69 / 100, size * 69 / 100), 255);
            }
        }
        // Each state looks different.
        let images: Vec<_> = FolderState::ALL
            .iter()
            .map(|&s| folder_rgba(s, 32))
            .collect();
        for (i, a) in images.iter().enumerate() {
            assert!(images[i + 1..].iter().all(|b| a != b));
        }
    }

    #[test]
    fn ico_layout() {
        let images: Vec<_> = FOLDER_SIZES
            .iter()
            .map(|&s| (s, folder_rgba(FolderState::Dirty, s)))
            .collect();
        let ico = encode_ico(&images);
        let u16_at = |i: usize| u16::from_le_bytes([ico[i], ico[i + 1]]);
        let u32_at = |i: usize| u32::from_le_bytes(ico[i..i + 4].try_into().unwrap());
        assert_eq!((u16_at(0), u16_at(2), u16_at(4)), (0, 1, 6));
        let mut expected_offset = 6 + 16 * 6;
        for (n, &size) in FOLDER_SIZES.iter().enumerate() {
            let e = 6 + 16 * n;
            let dim = if size == 256 { 0 } else { size as u8 };
            assert_eq!((ico[e], ico[e + 1]), (dim, dim));
            assert_eq!((u16_at(e + 4), u16_at(e + 6)), (1, 32));
            let (len, offset) = (u32_at(e + 8) as usize, u32_at(e + 12) as usize);
            assert_eq!(offset, expected_offset);
            assert!(offset + len <= ico.len());
            // BITMAPINFOHEADER: double height for the AND mask.
            assert_eq!(u32_at(offset), 40);
            assert_eq!(u32_at(offset + 4), size);
            assert_eq!(u32_at(offset + 8), size * 2);
            let mask_row = size.div_ceil(32) * 4;
            assert_eq!(len as u32, 40 + size * size * 4 + mask_row * size);
            expected_offset += len;
        }
        assert_eq!(expected_offset, ico.len());
        // Bottom-up BGRA: the first pixel is the image's bottom-left one.
        let rgba = &images[0].1;
        let bottom_left = (15 * 16 * 4) as usize;
        let first = 6 + 16 * 6 + 40;
        assert_eq!(ico[first + 3], rgba[bottom_left + 3]);
        assert_eq!(folder_ico(FolderState::Dirty), ico, "deterministic");
    }
}
