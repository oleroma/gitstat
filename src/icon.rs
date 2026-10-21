//! Draws the tray icon: a green disc with a checkmark when everything is in
//! sync, or a red disc showing how many repos aren't. Also draws the folder
//! icons Explorer shows on each repo, and packs them into .ico files.
//!
//! Every image is drawn at the exact pixel size it is shown at, with
//! anti-aliased edges: Windows scaling a larger icon down is what makes it
//! look jagged. The tray's text comes from Segoe UI Bold when Windows has
//! it, else from a built-in stroke font.

use crate::git::RepoStatus;
use ab_glyph::{Font, FontVec, PxScale};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// The tray icon's size at 100% scale. tray.rs asks Windows for the size it
/// actually uses (20 at 125%, 24 at 150%, 32 at 200%).
#[cfg_attr(not(windows), allow(dead_code))]
pub const TRAY_SIZE: u32 = 16;
/// The sizes `render` draws; anything else is clamped into this range.
const TRAY_SIZES: (u32, u32) = (8, 256);
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

/// The text a badge shows for `count` unsynced repos: three characters at most.
pub fn count_text(count: usize) -> String {
    if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    }
}

/// Segoe UI Bold, else Semibold, from the Windows fonts folder, read once.
/// `None` where neither exists (Linux, a stripped-down Windows).
pub fn system_font() -> Option<&'static FontVec> {
    static FONT: OnceLock<Option<FontVec>> = OnceLock::new();
    FONT.get_or_init(|| {
        let fonts = std::path::PathBuf::from(std::env::var_os("WINDIR")?).join("Fonts");
        ["segoeuib.ttf", "seguisb.ttf"].iter().find_map(|name| {
            let bytes = std::fs::read(fonts.join(name)).ok()?;
            FontVec::try_from_vec(bytes).ok()
        })
    })
    .as_ref()
}

/// Samples per pixel along each axis for shapes drawn by point tests: 64
/// coverage levels at tray and small folder sizes, fewer where a pixel is a
/// small part of an edge.
fn supersample_for(size: u32) -> u32 {
    match size {
        0..=64 => 8,
        65..=127 => 4,
        _ => 2,
    }
}

/// Returns `size`×`size` straight (non-premultiplied) RGBA pixels, with the
/// system font when it loads.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn render(badge: Badge, size: u32) -> Vec<u8> {
    render_with(badge, size, system_font())
}

fn render_with(badge: Badge, size: u32, font: Option<&FontVec>) -> Vec<u8> {
    let size = size.clamp(TRAY_SIZES.0, TRAY_SIZES.1);
    let center = size as f32 / 2.0;
    // Half a pixel in, so the outermost pixels are a soft edge, not a cut.
    let radius = center - 0.5;
    let disc = coverage(size, |x, y| {
        (x - center).powi(2) + (y - center).powi(2) <= radius * radius
    });
    let mut fg = vec![0.0; disc.len()];
    let color = match badge {
        Badge::Checking => {
            draw_text(&mut fg, size, "…", font);
            GRAY
        }
        Badge::AllSynced => {
            let s = size as f32 / 32.0;
            let points = [
                (9.0 * s, 16.5 * s),
                (14.0 * s, 22.0 * s),
                (23.5 * s, 11.0 * s),
            ];
            let width = (3.6 * s).max(1.6);
            draw_strokes(&mut fg, size, &[points.to_vec()], width / 2.0);
            GREEN
        }
        Badge::Problem => {
            draw_text(&mut fg, size, "!", font);
            RED
        }
        Badge::Unsynced(count) => {
            draw_text(&mut fg, size, &count_text(count), font);
            RED
        }
    };
    compose(&disc, &fg, color)
}

/// Straight RGBA from the disc's coverage and the white text's coverage.
fn compose(disc: &[f32], fg: &[f32], color: [u8; 3]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(disc.len() * 4);
    for (&alpha, &text) in disc.iter().zip(fg) {
        // Coverages are shares of the whole pixel. Straight alpha wants the
        // colour of the visible part: the text's share of the disc's part.
        let white = if alpha > 0.0 {
            (text / alpha).min(1.0)
        } else {
            0.0
        };
        for c in color {
            rgba.push((c as f32 + (255.0 - c as f32) * white).round() as u8);
        }
        rgba.push((alpha.clamp(0.0, 1.0) * 255.0).round() as u8);
    }
    rgba
}

/// Per-pixel coverage of a shape given as a point test in pixel coordinates
/// (y down, (0, 0) the top-left corner of the image).
fn coverage(size: u32, inside: impl Fn(f32, f32) -> bool) -> Vec<f32> {
    coverage_within(size, (0, 0, size, size), inside)
}

/// `coverage`, sampling only the pixels in `area` (left, top, right,
/// bottom); the rest are 0.
fn coverage_within(
    size: u32,
    area: (u32, u32, u32, u32),
    inside: impl Fn(f32, f32) -> bool,
) -> Vec<f32> {
    let ss = supersample_for(size);
    let samples = (ss * ss) as f32;
    let mut out = vec![0.0; (size * size) as usize];
    for y in area.1..area.3.min(size) {
        for x in area.0..area.2.min(size) {
            let mut hits = 0u32;
            for sy in 0..ss {
                for sx in 0..ss {
                    let px = x as f32 + (sx as f32 + 0.5) / ss as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / ss as f32;
                    hits += u32::from(inside(px, py));
                }
            }
            out[(y * size + x) as usize] = hits as f32 / samples;
        }
    }
    out
}

/// Adds round-capped polylines (pixel coordinates, half-width `r`) to `mask`.
fn draw_strokes(mask: &mut [f32], size: u32, lines: &[Vec<(f32, f32)>], r: f32) {
    let points = || lines.iter().flatten();
    let px = |v: f32| v.clamp(0.0, size as f32) as u32;
    let area = (
        px(points().map(|p| p.0).fold(f32::INFINITY, f32::min) - r - 1.0),
        px(points().map(|p| p.1).fold(f32::INFINITY, f32::min) - r - 1.0),
        px(points().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max) + r + 2.0),
        px(points().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max) + r + 2.0),
    );
    let ink = coverage_within(size, area, |x, y| {
        lines.iter().any(|line| {
            line.windows(2)
                .any(|seg| distance_to_segment((x, y), seg[0], seg[1]) <= r)
        })
    });
    for (m, c) in mask.iter_mut().zip(ink) {
        *m = (*m + c).min(1.0);
    }
}

/// Ink box (left, top, right, bottom), y down.
type Ink = [f32; 4];

fn union(a: Option<Ink>, b: Ink) -> Ink {
    match a {
        None => b,
        Some(a) => [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ],
    }
}

/// The largest the text may be, as shares of the icon (width, height):
/// one or two digits stay tall enough to read at 16 px, and three
/// characters ("99+") get the full width of the disc's middle.
fn text_box(chars: usize) -> (f32, f32) {
    match chars {
        0 | 1 => (0.8, 0.625),
        2 => (0.78, 0.5625),
        _ => (0.84, 0.47),
    }
}

/// Scale (px per text unit) and where the text's origin goes, centring
/// `ink` (in text units) in a `size` icon. The ink height snaps to whole
/// pixels and its top and left to pixel edges, so baselines, flat tops and
/// narrow stems such as "!" are crisp; centring is off by half a pixel at most.
fn fit(ink: Ink, size: u32, chars: usize) -> Option<(f32, f32, f32)> {
    let (w, h) = (ink[2] - ink[0], ink[3] - ink[1]);
    if !(w > 0.0 && h > 0.0) {
        return None;
    }
    let n = size as f32;
    let (max_w, max_h) = text_box(chars);
    let scale = (n * max_w / w).min(n * max_h / h);
    let height = (h * scale).floor().max(1.0);
    let scale = height / h;
    let top = ((n - height) / 2.0).floor();
    let left = ((n - w * scale) / 2.0).round();
    Some((scale, left - ink[0] * scale, top - ink[1] * scale))
}

/// Draws `text` centred in white, with `font` when it has every character,
/// else with the stroke font.
fn draw_text(mask: &mut [f32], size: u32, text: &str, font: Option<&FontVec>) {
    let chars: Vec<char> = text.chars().collect();
    if let Some(font) = font
        && font_text(mask, size, &chars, font)
    {
        return;
    }
    stroke_text(mask, size, &chars);
}

/// Letter spacing for fonts, in em: digits a little tighter than the
/// font's, so two of them fit a 16 px disc at a readable height.
const TRACKING: f32 = -0.03;

/// False when `font` lacks one of `chars` or has no ink for them.
fn font_text(mask: &mut [f32], size: u32, chars: &[char], font: &FontVec) -> bool {
    let em = font.units_per_em().unwrap_or(1000.0);
    let mut glyphs = Vec::new();
    let (mut pen, mut prev, mut ink) = (0.0f32, None, None);
    // Font units, y down, from the first glyph's origin on the baseline.
    for &c in chars {
        let id = font.glyph_id(c);
        if id.0 == 0 {
            return false;
        }
        if let Some(prev) = prev {
            pen += font.kern_unscaled(prev, id);
        }
        if let Some(outline) = font.outline(id) {
            // `bounds` is y up: min is (x_min, y_max), max is (x_max, y_min).
            let b = outline.bounds;
            ink = Some(union(
                ink,
                [pen + b.min.x, -b.min.y, pen + b.max.x, -b.max.y],
            ));
        }
        glyphs.push((id, pen));
        pen += font.h_advance_unscaled(id) + TRACKING * em;
        prev = Some(id);
    }
    let Some((scale, ox, oy)) = ink.and_then(|ink| fit(ink, size, chars.len())) else {
        return false;
    };
    // ab_glyph's scale is the pixel height of ascent minus descent.
    let px = PxScale::from(scale * font.height_unscaled());
    let n = size as i32;
    for (id, x) in glyphs {
        let glyph = id.with_scale_and_position(px, ab_glyph::point(ox + x * scale, oy));
        let Some(outlined) = font.outline_glyph(glyph) else {
            continue;
        };
        let b = outlined.px_bounds();
        let (left, top) = (b.min.x as i32, b.min.y as i32);
        outlined.draw(|gx, gy, c| {
            let (x, y) = (left + gx as i32, top + gy as i32);
            if (0..n).contains(&x) && (0..n).contains(&y) {
                let m = &mut mask[(y * n + x) as usize];
                *m = (*m + c).min(1.0);
            }
        });
    }
    true
}

/// The stroke font's line width, in digit heights.
const STROKE: f32 = 0.2;
/// Space between stroke glyphs' ink, in digit heights.
const STROKE_GAP: f32 = 0.14;

/// Points on an elliptical arc, angles in degrees clockwise from +x (y is down).
fn arc(cx: f32, cy: f32, rx: f32, ry: f32, from: f32, to: f32) -> Vec<(f32, f32)> {
    let steps = ((to - from).abs() / 10.0).ceil().max(2.0) as usize;
    (0..=steps)
        .map(|i| {
            let a = (from + (to - from) * i as f32 / steps as f32).to_radians();
            (cx + rx * a.cos(), cy + ry * a.sin())
        })
        .collect()
}

/// The built-in font: centre lines of round-capped strokes, one digit
/// high (y down from 0 to 1). A one-point line is a dot.
fn stroke_glyph(c: char) -> Vec<Vec<(f32, f32)>> {
    let dot = |x: f32, y: f32| vec![(x, y), (x, y)];
    let six = || {
        vec![
            arc(0.29, 0.68, 0.27, 0.32, 0.0, 360.0),
            arc(0.58, 0.68, 0.56, 0.68, 262.0, 180.0),
        ]
    };
    match c {
        '0' => vec![arc(0.28, 0.5, 0.28, 0.5, 0.0, 360.0)],
        '1' => vec![vec![(0.04, 0.22), (0.3, 0.0), (0.3, 1.0)]],
        '2' => {
            let mut line = arc(0.28, 0.28, 0.28, 0.28, 195.0, 370.0);
            line.extend([(0.0, 1.0), (0.58, 1.0)]);
            vec![line]
        }
        '3' => vec![
            arc(0.27, 0.25, 0.25, 0.25, 200.0, 450.0),
            arc(0.28, 0.735, 0.28, 0.265, 270.0, 515.0),
        ],
        '4' => vec![vec![(0.44, 1.0), (0.44, 0.0), (0.0, 0.7), (0.6, 0.7)]],
        '5' => {
            let mut line = vec![(0.52, 0.0), (0.1, 0.0), (0.06, 0.45)];
            line.extend(arc(0.28, 0.68, 0.28, 0.32, 235.0, 500.0));
            vec![line]
        }
        '6' => six(),
        '7' => vec![vec![(0.0, 0.0), (0.56, 0.0), (0.2, 1.0)]],
        '8' => vec![
            arc(0.28, 0.245, 0.235, 0.245, 0.0, 360.0),
            arc(0.28, 0.735, 0.28, 0.265, 0.0, 360.0),
        ],
        // A 6 turned upside down.
        '9' => six()
            .into_iter()
            .map(|line| line.into_iter().map(|(x, y)| (0.58 - x, 1.0 - y)).collect())
            .collect(),
        '+' => vec![
            vec![(0.28, 0.22), (0.28, 0.78)],
            vec![(0.0, 0.5), (0.56, 0.5)],
        ],
        '!' => vec![vec![(0.0, 0.0), (0.0, 0.6)], dot(0.0, 1.0)],
        '…' => vec![dot(0.0, 1.0), dot(0.4, 1.0), dot(0.8, 1.0)],
        _ => Vec::new(),
    }
}

fn stroke_text(mask: &mut [f32], size: u32, chars: &[char]) {
    let half = STROKE / 2.0;
    let mut lines: Vec<Vec<(f32, f32)>> = Vec::new();
    let (mut pen, mut ink) = (0.0f32, None);
    for &c in chars {
        let glyph = stroke_glyph(c);
        let points = || glyph.iter().flatten();
        let left = points().map(|p| p.0).fold(f32::INFINITY, f32::min);
        let right = points().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max);
        let top = points().map(|p| p.1).fold(f32::INFINITY, f32::min);
        let bottom = points().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);
        if !left.is_finite() {
            pen += 0.5;
            continue;
        }
        let dx = pen + half - left;
        ink = Some(union(
            ink,
            [pen, top - half, pen + right - left + STROKE, bottom + half],
        ));
        lines.extend(
            glyph
                .iter()
                .map(|line| line.iter().map(|&(x, y)| (x + dx, y)).collect()),
        );
        pen += right - left + STROKE + STROKE_GAP;
    }
    let Some((scale, ox, oy)) = ink.and_then(|ink| fit(ink, size, chars.len())) else {
        return;
    };
    let lines: Vec<Vec<(f32, f32)>> = lines
        .into_iter()
        .map(|line| {
            line.into_iter()
                .map(|(x, y)| (ox + x * scale, oy + y * scale))
                .collect()
        })
        .collect();
    draw_strokes(mask, size, &lines, half * scale);
}

fn distance_to_segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let len2 = abx * abx + aby * aby;
    // A zero-length segment is a dot.
    let t = if len2 > 0.0 {
        (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (cx, cy) = (a.0 + t * abx, a.1 + t * aby);
    ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt()
}

/// Bump whenever the folder artwork changes: Explorer caches icons by path,
/// so new pixels need a new file name.
pub const ICON_VERSION: u32 = 2;
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

    /// The colour a repo's own icon takes on (folder icon mode `repo-svg`).
    pub fn tone(self) -> Tone {
        match self {
            Self::Synced => Tone::Synced,
            Self::Dirty | Self::Ahead | Self::NoUpstream => Tone::Local,
            Self::Behind | Self::Diverged | Self::Error => Tone::Attention,
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

/// The three colours a repo's own icon is drawn in: fewer than the status
/// folders' seven, so it reads at a glance whatever its shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Nothing to do.
    Synced,
    /// Work that is only here: uncommitted, unpushed, or no upstream.
    Local,
    /// Needs a pull or a look: behind, diverged, or an error.
    Attention,
}

impl Tone {
    #[cfg(test)]
    pub const ALL: [Tone; 3] = [Tone::Synced, Tone::Local, Tone::Attention];

    pub fn color(self) -> [u8; 3] {
        match self {
            Self::Synced => GREEN,
            Self::Local => AMBER,
            Self::Attention => RED,
        }
    }

    /// Stable name for file names.
    pub fn name(self) -> &'static str {
        match self {
            Self::Synced => "green",
            Self::Local => "amber",
            Self::Attention => "red",
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
    let ss = supersample_for(size);
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

    const BADGES: [Badge; 7] = [
        Badge::Checking,
        Badge::AllSynced,
        Badge::Unsynced(3),
        Badge::Unsynced(42),
        Badge::Unsynced(88),
        Badge::Unsynced(150),
        Badge::Problem,
    ];
    /// 100% to 300%, as GetSystemMetricsForDpi(SM_CXSMICON) reports them.
    const TRAY_PX: [u32; 7] = [16, 20, 24, 28, 32, 40, 48];

    /// A real TrueType font, to cover the font path where Segoe UI is
    /// missing: Windows has Segoe UI, most Linux CI images Liberation Sans.
    fn any_font() -> Option<&'static FontVec> {
        static FONT: OnceLock<Option<FontVec>> = OnceLock::new();
        FONT.get_or_init(|| {
            [
                "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
            ]
            .iter()
            .find_map(|f| FontVec::try_from_vec(std::fs::read(f).ok()?).ok())
        })
        .as_ref()
        .or_else(system_font)
    }

    fn alpha(px: &[u8], size: u32, x: u32, y: u32) -> u8 {
        px[((y * size + x) * 4 + 3) as usize]
    }

    /// How white a pixel is over the disc colour, 0 to 1.
    fn whiteness(px: &[u8], size: u32, x: u32, y: u32, color: [u8; 3]) -> f32 {
        let i = ((y * size + x) * 4) as usize;
        (px[i + 1] as f32 - color[1] as f32) / (255.0 - color[1] as f32)
    }

    #[test]
    fn renders_every_badge_at_every_tray_size() {
        for font in [None, any_font()] {
            for size in TRAY_PX {
                for badge in BADGES {
                    let px = render_with(badge, size, font);
                    let ctx = format!("{badge:?} at {size} px, font {}", font.is_some());
                    assert_eq!(px.len(), (size * size * 4) as usize, "{ctx}");
                    // Corners are transparent, the center is opaque.
                    assert_eq!(alpha(&px, size, 0, 0), 0, "{ctx}");
                    assert_eq!(alpha(&px, size, size - 1, size - 1), 0, "{ctx}");
                    assert_eq!(alpha(&px, size, size / 2, size / 2), 255, "{ctx}");
                    // The rim is anti-aliased, not cut: partial alpha on
                    // the row through the centre.
                    let rim = alpha(&px, size, 0, size / 2);
                    assert!(rim > 0 && rim < 255, "{ctx}: rim alpha {rim}");
                    // The text or mark is there, with soft edges too.
                    let color = match badge {
                        Badge::AllSynced => GREEN,
                        Badge::Checking => GRAY,
                        _ => RED,
                    };
                    let levels: Vec<f32> = (0..size * size)
                        .map(|i| whiteness(&px, size, i % size, i / size, color))
                        .collect();
                    // The 16 px "…" is three 2 px dots that needn't hit whole pixels.
                    let solid = if badge == Badge::Checking { 0.7 } else { 0.95 };
                    assert!(levels.iter().any(|&w| w > solid), "{ctx}: no solid white");
                    assert!(
                        levels.iter().any(|&w| w > 0.1 && w < 0.9),
                        "{ctx}: no anti-aliased text pixels"
                    );
                }
            }
        }
        // Out-of-range sizes are clamped rather than panicking.
        assert_eq!(render_with(Badge::Problem, 0, None).len(), 8 * 8 * 4);
        assert_eq!(render_with(Badge::Problem, 4096, None).len(), 256 * 256 * 4);
    }

    #[test]
    fn edges_keep_straight_colour() {
        // Straight alpha: a half-covered rim pixel is the disc's colour at
        // half alpha, never the colour darkened by premultiplying.
        for font in [None, any_font()] {
            for size in TRAY_PX {
                let px = render_with(Badge::Unsynced(7), size, font);
                for i in 0..(size * size) as usize {
                    let a = px[i * 4 + 3];
                    let (x, y) = ((i as u32 % size) as f32, (i as u32 / size) as f32);
                    let c = size as f32 / 2.0;
                    let d = ((x + 0.5 - c).powi(2) + (y + 0.5 - c).powi(2)).sqrt();
                    if a > 0 && d > c - 2.0 {
                        assert_eq!(px[i * 4..i * 4 + 3], RED, "{size}: pixel {i}, alpha {a}");
                    }
                    if a == 0 {
                        assert_eq!(px[i * 4..i * 4 + 3], RED, "{size}: transparent {i}");
                    }
                }
            }
        }
    }

    /// The rows and columns holding text (whiteness over half).
    fn text_box_px(px: &[u8], size: u32, color: [u8; 3]) -> (u32, u32, u32, u32) {
        let (mut x0, mut y0, mut x1, mut y1) = (size, size, 0, 0);
        for y in 0..size {
            for x in 0..size {
                if whiteness(px, size, x, y, color) > 0.5 {
                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
                }
            }
        }
        (x0, y0, x1, y1)
    }

    #[test]
    fn counts_are_legible_and_inside_the_disc() {
        for font in [None, any_font()] {
            for size in TRAY_PX {
                for (count, min_share) in [(1, 0.5), (7, 0.5), (42, 0.45), (88, 0.45), (100, 0.3)] {
                    let px = render_with(Badge::Unsynced(count), size, font);
                    let ctx = format!("{count} at {size} px, font {}", font.is_some());
                    let (x0, y0, x1, y1) = text_box_px(&px, size, RED);
                    let height = (y1 - y0) as f32;
                    assert!(height >= size as f32 * min_share, "{ctx}: {height} px tall");
                    // Centred: equal margins give or take a pixel.
                    let (top, bottom) = (y0 as i32, (size - y1) as i32);
                    assert!(
                        (top - bottom).abs() <= 2,
                        "{ctx}: top {top}, bottom {bottom}"
                    );
                    let (left, right) = (x0 as i32, (size - x1) as i32);
                    assert!(
                        (left - right).abs() <= 2,
                        "{ctx}: left {left}, right {right}"
                    );
                    // Every white pixel is well inside the disc.
                    let c = size as f32 / 2.0;
                    for y in 0..size {
                        for x in 0..size {
                            if whiteness(&px, size, x, y, RED) > 0.05 {
                                let d = ((x as f32 + 0.5 - c).powi(2)
                                    + (y as f32 + 0.5 - c).powi(2))
                                .sqrt();
                                assert!(d < c - 0.5, "{ctx}: text at {x},{y}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn every_count_looks_different() {
        let images: Vec<_> = (1..=100)
            .map(|n| render_with(Badge::Unsynced(n), TRAY_SIZE, None))
            .collect();
        for (i, a) in images.iter().enumerate() {
            assert!(
                images[i + 1..].iter().all(|b| a != b),
                "{} looks like a later count",
                i + 1
            );
        }
        assert_eq!(count_text(99), "99");
        assert_eq!(count_text(100), "99+");
        assert_eq!(
            render_with(Badge::Unsynced(150), 20, None),
            render_with(Badge::Unsynced(100), 20, None)
        );
    }

    #[test]
    fn stroke_font_covers_every_badge_character() {
        for c in "0123456789+!…".chars() {
            assert!(!stroke_glyph(c).is_empty(), "{c:?}");
        }
        // Unknown characters draw nothing, and text of them draws nothing.
        assert!(stroke_glyph('x').is_empty());
        let mut mask = vec![0.0; 16 * 16];
        stroke_text(&mut mask, 16, &['x']);
        assert!(mask.iter().all(|&m| m == 0.0));
    }

    #[test]
    fn fit_snaps_height_and_centres() {
        // A 1×2 ink box in a 16 px icon, one character: height-limited to
        // 10 px, top on a pixel edge, centred across.
        let (scale, ox, oy) = fit([0.0, 0.0, 1.0, 2.0], 16, 1).unwrap();
        assert_eq!(scale, 5.0);
        assert_eq!(oy, 3.0);
        assert_eq!(ox, 6.0);
        // Wide text is width-limited, and still snaps to whole pixels.
        let ink = [0.5, -1.0, 4.5, 0.0];
        let (scale, ox, oy) = fit(ink, 20, 3).unwrap();
        let height = (ink[3] - ink[1]) * scale;
        assert_eq!(height, height.round());
        assert!((ink[2] - ink[0]) * scale <= 20.0 * 0.84);
        assert_eq!((oy + ink[1] * scale).fract(), 0.0);
        let (left, right) = (ox + ink[0] * scale, ox + ink[2] * scale);
        assert_eq!(left.fract(), 0.0);
        assert!((left - (20.0 - right)).abs() <= 1.0);
        assert_eq!(fit([0.0, 0.0, 0.0, 1.0], 16, 1), None);
        assert_eq!(fit([0.0, 0.0, f32::NAN, 1.0], 16, 1), None);
    }

    #[test]
    fn missing_glyphs_fall_back_to_strokes() {
        let Some(font) = any_font() else { return };
        // A private-use character no font has: the whole text uses strokes.
        let mut with_font = vec![0.0; 16 * 16];
        assert!(!font_text(&mut with_font, 16, &['\u{F8FF}'], font));
        assert!(with_font.iter().all(|&m| m == 0.0));
        assert!(font_text(&mut with_font, 16, &['4', '2'], font));
        assert!(with_font.iter().any(|&m| m > 0.9));
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
    fn tones_group_the_states() {
        use FolderState::*;
        let tones: Vec<_> = FolderState::ALL.iter().map(|s| (*s, s.tone())).collect();
        assert_eq!(
            tones,
            [
                (Synced, Tone::Synced),
                (Ahead, Tone::Local),
                (Behind, Tone::Attention),
                (Diverged, Tone::Attention),
                (Dirty, Tone::Local),
                (NoUpstream, Tone::Local),
                (Error, Tone::Attention),
            ]
        );
        // The status palette: green, amber, red.
        assert_eq!(Tone::Synced.color(), GREEN);
        assert_eq!(Tone::Local.color(), AMBER);
        assert_eq!(Tone::Attention.color(), RED);
        let names: std::collections::HashSet<_> = Tone::ALL.iter().map(|t| t.name()).collect();
        assert_eq!(names.len(), 3);
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
