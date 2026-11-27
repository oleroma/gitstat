//! Words on whole device px, the same in every window.
//!
//! GPUI places a glyph by multiplying its logical place by the scale and
//! flooring: a line whose box sits on a whole device px can come out a
//! quarter px left or a whole px up in one window and not in another,
//! by a rounding error in the last bit. So the words here are placed in
//! device px by this module: the line's box is rounded to its device px
//! once, every glyph's place in the line is worked out from the line
//! alone, and GPUI is handed places it can only read one way. The same
//! words then come out as the same pixels wherever their box is.
//!
//! [`paint`] draws a line for a part the scene places; [`text`] is the
//! element for words inside ordinary GPUI layouts.

use gpui::{
    App, AvailableSpace, Bounds, Element, ElementId, FontWeight, GlobalElementId, Hsla,
    InspectorElementId, IntoElement, LayoutId, LineLayout, Pixels, SharedString, Style, TextAlign,
    TextRun, TextStyle, WhiteSpace, Window, point, px, size,
};

/// Past the place GPUI is handed, device px: it floors what it is given,
/// and picks one of four glyph pictures by the quarter px, so the place
/// is kept clear of both edges of its quarter.
const NUDGE_X: f32 = 0.06;
const NUDGE_Y: f32 = 0.25;

/// How a part's words look.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    /// Their height (the em), logical px.
    pub size: f32,
    pub weight: f32,
    pub color: Hsla,
    /// Their font, when not the window's own.
    pub family: Option<&'static str>,
}

impl Look {
    pub fn new(size: f32, color: Hsla) -> Self {
        Self {
            size,
            weight: 400.0,
            color,
            family: None,
        }
    }

    pub fn bold(mut self) -> Self {
        self.weight = 600.0;
        self
    }

    pub fn family(mut self, family: &'static str) -> Self {
        self.family = Some(family);
        self
    }
}

/// Where a line sits in its box, sideways.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Centre,
}

fn run(len: usize, look: &Look, window: &Window) -> TextRun {
    let mut font = window.text_style().font();
    font.weight = FontWeight(look.weight);
    if let Some(family) = look.family {
        font.family = family.into();
    }
    TextRun {
        len,
        font,
        color: look.color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// How wide `text` runs, logical px.
pub fn width(text: &str, look: &Look, window: &Window) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    let line = window.text_system().shape_line(
        SharedString::new(text),
        px(look.size),
        &[run(text.len(), look, window)],
        None,
    );
    f32::from(line.width)
}

/// `text` cut with an ellipsis to fit `max` logical px, as GPUI cuts it.
fn cut(
    text: &SharedString,
    max: f32,
    run: &TextRun,
    size: Pixels,
    window: &Window,
) -> SharedString {
    let mut wrapper = window.text_system().line_wrapper(run.font.clone(), size);
    let mut runs = vec![run.clone()];
    wrapper.truncate_line(text.clone(), px(max), "…", &mut runs)
}

/// Paints `text` as one line in the line box whose top-left corner is at
/// the device px `at` (from the window's corner), `line_height` logical px
/// tall. Wider than `max` device px, it is cut with an ellipsis; `align`
/// places it across `max`.
#[allow(clippy::too_many_arguments)]
pub fn paint(
    window: &mut Window,
    text: &SharedString,
    look: &Look,
    at: (i32, i32),
    line_height: f32,
    max: Option<i32>,
    align: Align,
) {
    if text.is_empty() || look.color.a <= 0.0 {
        return;
    }
    let s = window.scale_factor();
    let size = px(look.size);
    let run = run(text.len(), look, window);
    let mut line =
        window
            .text_system()
            .shape_line(text.clone(), size, std::slice::from_ref(&run), None);
    if let Some(max) = max
        && f32::from(line.width) * s > max as f32 + 0.5
    {
        let text = cut(text, max as f32 / s, &run, size, window);
        let run = TextRun {
            len: text.len(),
            ..run
        };
        line = window.text_system().shape_line(text, size, &[run], None);
    }
    let shift = match (align, max) {
        (Align::Centre, Some(max)) => (max as f32 / s - f32::from(line.width)) / 2.0,
        _ => 0.0,
    };
    glyphs(
        window,
        &line,
        0..usize::MAX,
        (at.0, at.1 + baseline(&line, line_height, s)),
        shift,
        look.color,
    );
}

/// A line's baseline below its box's top, device px: where GPUI puts it,
/// floored as GPUI floors it.
fn baseline(line: &LineLayout, line_height: f32, s: f32) -> i32 {
    let (ascent, descent) = (f32::from(line.ascent), f32::from(line.descent));
    let top = (line_height - ascent - descent) / 2.0;
    ((top + ascent) * s).floor() as i32
}

/// Paints the glyphs of `line` numbered `which` (counted across its
/// runs), the line's start at device px `x`, its baseline at `y`.
/// `shift` moves them sideways, logical px.
fn glyphs(
    window: &mut Window,
    line: &LineLayout,
    which: std::ops::Range<usize>,
    (x, y): (i32, i32),
    shift: f32,
    color: Hsla,
) {
    let s = window.scale_factor();
    let mut n = 0;
    for run in &line.runs {
        for glyph in &run.glyphs {
            let i = n;
            n += 1;
            if !which.contains(&i) {
                continue;
            }
            // To the quarter px, as GPUI draws glyphs: worked out from
            // the line alone, so it is the same wherever the line is.
            let local = (f32::from(glyph.position.x) + shift) * s;
            let quarter = (local * 4.0).floor() / 4.0;
            let at = point(
                px((x as f32 + quarter + NUDGE_X) / s),
                px((y as f32 + NUDGE_Y) / s),
            );
            if glyph.is_emoji {
                window
                    .paint_emoji(at, run.font_id, glyph.id, line.font_size)
                    .ok();
            } else {
                window
                    .paint_glyph(at, run.font_id, glyph.id, line.font_size, color)
                    .ok();
            }
        }
    }
}

/// Words inside an ordinary GPUI layout, in the text style around them:
/// one line, cut with an ellipsis where the style says so, or wrapped to
/// the width they are given.
pub fn text(text: impl Into<SharedString>) -> Text {
    Text { text: text.into() }
}

pub struct Text {
    text: SharedString,
}

impl IntoElement for Text {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

/// Whether words in `style` wrap rather than stay on one line.
fn wraps(style: &TextStyle) -> bool {
    style.white_space == WhiteSpace::Normal && style.text_overflow.is_none()
}

impl Element for Text {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        _: &mut App,
    ) -> (LayoutId, ()) {
        let style = window.text_style();
        let rem = window.rem_size();
        let font_size = style.font_size.to_pixels(rem);
        let line_height = style.line_height_in_pixels(rem);
        let text = self.text.clone();
        let id =
            window.request_measured_layout(Style::default(), move |known, available, window, _| {
                let limit = known.width.or(match available.width {
                    AvailableSpace::Definite(w) => Some(w),
                    _ => None,
                });
                let run = style.to_run(text.len());
                // The wrapper adds up letters one by one, which can come
                // out wider than the line as shaped: words that fit as
                // shaped stay on one line.
                let whole = window.text_system().shape_line(
                    text.clone(),
                    font_size,
                    std::slice::from_ref(&run),
                    None,
                );
                let fits = !text.contains('\n')
                    && limit.is_none_or(|limit| whole.width <= limit + px(0.5));
                if wraps(&style) && !fits {
                    let lines = window
                        .text_system()
                        .shape_text(text.clone(), font_size, &[run], limit, None)
                        .unwrap_or_default();
                    let mut width = px(0.0);
                    let mut rows = 0;
                    for line in &lines {
                        width = width.max(line.width());
                        rows += line.wrap_boundaries.len() + 1;
                    }
                    size(width.ceil(), line_height * rows.max(1))
                } else {
                    let line = whole;
                    let width = match limit {
                        Some(limit) if style.text_overflow.is_some() => line.width.min(limit),
                        _ => line.width,
                    };
                    size(width.ceil(), line_height)
                }
            });
        (id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) {
        if self.text.is_empty() {
            return;
        }
        let style = window.text_style();
        let rem = window.rem_size();
        let s = window.scale_factor();
        let font_size = style.font_size.to_pixels(rem);
        let line_height = f32::from(style.line_height_in_pixels(rem));
        // The box is laid out on whole device px, give or take a rounding
        // error: rounded, it is the same px in every window.
        let dev = |v: Pixels| (f32::from(v) * s).round() as i32;
        let (x, y) = (dev(bounds.origin.x), dev(bounds.origin.y));
        let box_w = f32::from(bounds.size.width);
        let run = style.to_run(self.text.len());
        let aligned = |line_w: f32| match style.text_align {
            TextAlign::Left => 0.0,
            TextAlign::Center => (box_w - line_w) / 2.0,
            TextAlign::Right => box_w - line_w,
        };
        let whole = window.text_system().shape_line(
            self.text.clone(),
            font_size,
            std::slice::from_ref(&run),
            None,
        );
        // As it was measured: words that fit as shaped stay on one line.
        let fits = !self.text.contains('\n') && f32::from(whole.width) <= box_w + 0.5;
        if !wraps(&style) || fits {
            let mut line = whole;
            if style.text_overflow.is_some() && f32::from(line.width) > box_w + 0.5 {
                let text = cut(&self.text, box_w, &run, font_size, window);
                let run = TextRun {
                    len: text.len(),
                    ..run
                };
                line = window
                    .text_system()
                    .shape_line(text, font_size, &[run], None);
            }
            let shift = aligned(f32::from(line.width));
            let y = y + baseline(&line, line_height, s);
            glyphs(window, &line, 0..usize::MAX, (x, y), shift, style.color);
            return;
        }
        let Ok(lines) = window.text_system().shape_text(
            self.text.clone(),
            font_size,
            &[run],
            Some(bounds.size.width + px(0.5)),
            None,
        ) else {
            return;
        };
        let mut row = 0usize;
        for wrapped in &lines {
            let line = &wrapped.unwrapped_layout;
            let base = baseline(line, line_height, s);
            // Each wrapped row: its glyphs, and where its first one sits
            // in the unwrapped line.
            let count: usize = line.runs.iter().map(|r| r.glyphs.len()).sum();
            let flat = |b: &gpui::WrapBoundary| {
                line.runs[..b.run_ix]
                    .iter()
                    .map(|r| r.glyphs.len())
                    .sum::<usize>()
                    + b.glyph_ix
            };
            let mut starts = vec![0usize];
            starts.extend(wrapped.wrap_boundaries.iter().map(flat));
            starts.push(count);
            let position = |i: usize| {
                line.runs
                    .iter()
                    .flat_map(|r| r.glyphs.iter())
                    .nth(i)
                    .map_or(f32::from(line.width), |g| f32::from(g.position.x))
            };
            for pair in starts.windows(2) {
                let (from, to) = (pair[0], pair[1]);
                let start = position(from);
                let shift = aligned(position(to) - start) - start;
                let top = y + (row as f32 * line_height * s).round() as i32;
                glyphs(window, line, from..to, (x, top + base), shift, style.color);
                row += 1;
            }
        }
    }
}
