//! A view as a picture: a list of parts, each a box in whole device px
//! with what is painted in it.
//!
//! The panel and the big window each say what they show as such a list,
//! and paint themselves from it. The flip paints the list in between
//! ([`Picture::between`]): a part both views have (the same key, the same
//! words) is carried from one's box to the other's, growing and changing
//! colour on the way; a part only one has fades with its view. At either
//! end the list in between *is* that view's list, so the flip's first and
//! last frames are the views' own, painted by the same code.

use super::paint;
use super::panel::line_height;
use super::text::{self, Align, Look};
use crate::config::Section;
use crate::git::RepoStatus;
use crate::layout::Rect;
use gpui::{App, Bounds, ContentMask, Hsla, Pixels, Rgba, SharedString, Window, point, px, size};
use std::path::PathBuf;
use std::sync::Arc;

/// Which part of a view a part is: the same in both views for what the
/// flip carries between them.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// The title bar's fill: nothing tall in the panel.
    Caption,
    /// The line under the top bar.
    HeaderLine,
    /// The list's or the table's ground.
    Body,
    Headline,
    Subline,
    Health,
    Count(u8),
    Settings,
    /// A section's wash: its heading's in the panel, its spine's in the
    /// big window.
    Tint(Section),
    Icon(PathBuf),
    Name(PathBuf),
    Checkout(PathBuf),
    Status(PathBuf),
    /// A part only its own view has.
    Own,
}

/// What is painted in a part's box.
#[derive(Clone)]
pub enum What {
    /// A fill, its corners rounded: top left, top right, bottom right,
    /// bottom left, logical px.
    Fill { color: Hsla, radii: [f32; 4] },
    /// One line of words in its line box.
    Words {
        text: SharedString,
        look: Look,
        align: Align,
    },
    Health {
        counts: [usize; 3],
        colors: [Hsla; 3],
        track: Hsla,
        checking: bool,
    },
    /// A repo's icon, `side` logical px square, or a dot in `dot` while
    /// it has none. Part way between two sizes, `ends` are those: the
    /// picture of one is drawn at the size in between.
    Icon {
        status: Arc<RepoStatus>,
        side: f32,
        dot: Hsla,
        ends: Option<(f32, f32)>,
    },
    /// gitstat's own icon.
    AppIcon,
}

#[derive(Clone)]
pub struct Part {
    pub key: Key,
    /// Device px from the window's corner.
    pub rect: Rect,
    pub what: What,
    /// Painted over the view's own elements rather than under them.
    pub over: bool,
    /// Cut to the list or the table.
    pub clip: bool,
    pub alpha: f32,
    /// On its way between two views: its words, growing with their box,
    /// are cut only where they truly don't fit it.
    pub moving: bool,
}

impl Part {
    pub fn new(key: Key, rect: Rect, what: What) -> Self {
        let over = !matches!(what, What::Fill { .. });
        Self {
            key,
            rect,
            what,
            over,
            clip: false,
            alpha: 1.0,
            moving: false,
        }
    }

    pub fn fill(key: Key, rect: Rect, color: Hsla) -> Self {
        Self::new(
            key,
            rect,
            What::Fill {
                color,
                radii: [0.0; 4],
            },
        )
    }

    pub fn words(key: Key, rect: Rect, text: impl Into<SharedString>, look: Look) -> Self {
        Self::new(
            key,
            rect,
            What::Words {
                text: text.into(),
                look,
                align: Align::Left,
            },
        )
    }

    pub fn centred(mut self) -> Self {
        if let What::Words { align, .. } = &mut self.what {
            *align = Align::Centre;
        }
        self
    }

    pub fn radii(mut self, r: [f32; 4]) -> Self {
        if let What::Fill { radii, .. } = &mut self.what {
            *radii = r;
        }
        self
    }

    pub fn clipped(mut self) -> Self {
        self.clip = true;
        self
    }

    pub fn over(mut self) -> Self {
        self.over = true;
        self
    }
}

/// The sheet a view sits on, where gitstat draws it itself: the panel's
/// always, the big window's only in the flip, as Windows draws it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sheet {
    /// Its outside, rim and all, device px.
    pub frame: Rect,
    /// Its corners' radius, logical px.
    pub radius: f32,
    /// The rim's thickness, device px.
    pub rim: i32,
    pub rim_color: Hsla,
    pub bg: Hsla,
    /// How much of its shadow shows.
    pub shadow: f32,
}

/// Everything a view paints itself: its sheet, and its parts.
#[derive(Clone)]
pub struct Picture {
    pub sheet: Option<Sheet>,
    /// The list's or the table's box: what `clip` parts are cut to.
    pub body: Rect,
    pub parts: Vec<Part>,
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn lerp_px(a: i32, b: i32, t: f32) -> i32 {
    lerp(a as f32, b as f32, t).round() as i32
}

/// How far across and how far down a thing has travelled `t` of the way
/// through the turn: a quarter of a circle, not a straight line. The
/// panel's rows lie one under another and the window's columns side by
/// side, so going across first and up after keeps each row's words clear
/// of the next row's the whole way; a straight line would cross them.
pub fn sway(t: f32) -> (f32, f32) {
    let t = t.clamp(0.0, 1.0);
    if t <= 0.0 {
        return (0.0, 0.0);
    }
    if t >= 1.0 {
        return (1.0, 1.0);
    }
    let (sin, cos) = (t * std::f32::consts::FRAC_PI_2).sin_cos();
    (sin, 1.0 - cos)
}

/// `a` at 0, `b` at 1, every edge on a whole px in between, along the
/// turn's path ([`sway`]).
pub fn lerp_rect(a: Rect, b: Rect, t: f32) -> Rect {
    let (x, y) = sway(t);
    Rect::new(
        lerp_px(a.left, b.left, x),
        lerp_px(a.top, b.top, y),
        lerp_px(a.right, b.right, x),
        lerp_px(a.bottom, b.bottom, y),
    )
}

pub fn lerp_color(a: Hsla, b: Hsla, t: f32) -> Hsla {
    if a == b {
        return a;
    }
    let (a, b) = (a.to_rgb(), b.to_rgb());
    Rgba {
        r: lerp(a.r, b.r, t),
        g: lerp(a.g, b.g, t),
        b: lerp(a.b, b.b, t),
        a: lerp(a.a, b.a, t),
    }
    .into()
}

/// Whether the flip can carry `a` to `b`: the same kind of thing, saying
/// the same.
fn alike(a: &What, b: &What) -> bool {
    match (a, b) {
        (What::Fill { .. }, What::Fill { .. }) => true,
        (
            What::Words {
                text: ta,
                look: la,
                align: aa,
            },
            What::Words {
                text: tb,
                look: lb,
                align: ab,
            },
        ) => ta == tb && aa == ab && la.family == lb.family,
        (What::Health { counts: ca, .. }, What::Health { counts: cb, .. }) => ca == cb,
        (What::Icon { status: sa, .. }, What::Icon { status: sb, .. }) => sa.path == sb.path,
        (What::AppIcon, What::AppIcon) => true,
        _ => false,
    }
}

fn between_what(a: &What, b: &What, t: f32) -> What {
    match (a, b) {
        (
            What::Fill {
                color: ca,
                radii: ra,
            },
            What::Fill {
                color: cb,
                radii: rb,
            },
        ) => What::Fill {
            color: lerp_color(*ca, *cb, t),
            radii: [0, 1, 2, 3].map(|i| lerp(ra[i], rb[i], t)),
        },
        (
            What::Words {
                text,
                look: la,
                align,
            },
            What::Words { look: lb, .. },
        ) => What::Words {
            text: text.clone(),
            look: Look {
                // A quarter px and a hundred of weight at a time: only so
                // many sizes of each letter are ever drawn.
                size: (lerp(la.size, lb.size, t) * 4.0).round() / 4.0,
                weight: (lerp(la.weight, lb.weight, t) / 100.0).round() * 100.0,
                color: lerp_color(la.color, lb.color, t),
                family: la.family,
            },
            align: *align,
        },
        (
            What::Health {
                counts,
                colors: ca,
                track: ta,
                checking,
            },
            What::Health {
                colors: cb,
                track: tb,
                ..
            },
        ) => What::Health {
            counts: *counts,
            colors: [0, 1, 2].map(|i| lerp_color(ca[i], cb[i], t)),
            track: lerp_color(*ta, *tb, t),
            checking: *checking,
        },
        (
            What::Icon {
                status,
                side: sa,
                dot: da,
                ..
            },
            What::Icon {
                side: sb, dot: db, ..
            },
        ) => What::Icon {
            status: status.clone(),
            side: lerp(*sa, *sb, t),
            dot: lerp_color(*da, *db, t),
            ends: Some((*sa, *sb)),
        },
        _ => a.clone(),
    }
}

impl Sheet {
    fn between(a: &Sheet, b: &Sheet, t: f32) -> Sheet {
        Sheet {
            frame: lerp_rect(a.frame, b.frame, t),
            radius: lerp(a.radius, b.radius, t),
            rim: lerp_px(a.rim, b.rim, t),
            rim_color: lerp_color(a.rim_color, b.rim_color, t),
            bg: lerp_color(a.bg, b.bg, t),
            shadow: lerp(a.shadow, b.shadow, t),
        }
    }
}

impl Picture {
    /// The picture part way from `a` to `b`: what both have `t` of the
    /// way over, what only `a` has at `own_a` of its strength and what
    /// only `b` has at `own_b` of its. At `t` 0 with `own_a` 1 and
    /// `own_b` 0 it is `a` itself, and the other way round `b`.
    pub fn between(a: &Picture, b: &Picture, t: f32, own_a: f32, own_b: f32) -> Picture {
        let t = t.clamp(0.0, 1.0);
        // Each of `a`'s parts' twin in `b`, and the other way round.
        let mut to: Vec<Option<usize>> = vec![None; a.parts.len()];
        let mut from: Vec<Option<usize>> = vec![None; b.parts.len()];
        for (i, pa) in a
            .parts
            .iter()
            .enumerate()
            .filter(|(_, pa)| pa.key != Key::Own)
        {
            let twin = b.parts.iter().enumerate().position(|(j, pb)| {
                from[j].is_none() && pb.key == pa.key && alike(&pa.what, &pb.what)
            });
            if let Some(j) = twin {
                to[i] = Some(j);
                from[j] = Some(i);
            }
        }
        let carried = |pa: &Part, pb: &Part| {
            if t <= 0.0 {
                pa.clone()
            } else if t >= 1.0 {
                pb.clone()
            } else {
                Part {
                    key: pa.key.clone(),
                    rect: lerp_rect(pa.rect, pb.rect, t),
                    what: between_what(&pa.what, &pb.what, t),
                    over: pa.over,
                    clip: pa.clip || pb.clip,
                    alpha: lerp(pa.alpha, pb.alpha, t),
                    moving: true,
                }
            }
        };
        let own = |part: &Part, strength: f32| Part {
            alpha: part.alpha * strength,
            ..part.clone()
        };
        // In the order of the view it is nearer to, which at either end
        // makes it that view's own list.
        let mut parts = Vec::with_capacity(a.parts.len() + b.parts.len());
        if t < 0.5 {
            for (pa, twin) in a.parts.iter().zip(&to) {
                match twin {
                    Some(j) => parts.push(carried(pa, &b.parts[*j])),
                    None if own_a > 0.0 => parts.push(own(pa, own_a)),
                    None => {}
                }
            }
            if own_b > 0.0 {
                let rest = b.parts.iter().zip(&from).filter(|(_, twin)| twin.is_none());
                parts.extend(rest.map(|(pb, _)| own(pb, own_b)));
            }
        } else {
            for (pb, twin) in b.parts.iter().zip(&from) {
                match twin {
                    Some(i) => parts.push(carried(&a.parts[*i], pb)),
                    None if own_b > 0.0 => parts.push(own(pb, own_b)),
                    None => {}
                }
            }
            if own_a > 0.0 {
                let rest = a.parts.iter().zip(&to).filter(|(_, twin)| twin.is_none());
                parts.extend(rest.map(|(pa, _)| own(pa, own_a)));
            }
        }
        let sheet = match (&a.sheet, &b.sheet) {
            (Some(sa), Some(sb)) => Some(if t <= 0.0 {
                *sa
            } else if t >= 1.0 {
                *sb
            } else {
                Sheet::between(sa, sb, t)
            }),
            (Some(sa), None) => Some(*sa),
            (None, sb) => *sb,
        };
        Picture {
            sheet,
            body: if t <= 0.0 {
                a.body
            } else if t >= 1.0 {
                b.body
            } else {
                lerp_rect(a.body, b.body, t)
            },
            parts,
        }
    }

    /// How many pairs of travelling words lie over one another, and one
    /// such pair: for the self-test, which wants none.
    pub fn clashes(&self) -> (usize, Option<(String, String)>) {
        let words: Vec<(&Part, &str)> = self
            .parts
            .iter()
            .filter(|p| p.moving && p.alpha > 0.0)
            .filter_map(|p| match &p.what {
                What::Words { text, .. } if !text.is_empty() => Some((p, text.as_ref())),
                _ => None,
            })
            .collect();
        let mut n = 0;
        let mut one = None;
        for (i, (a, ta)) in words.iter().enumerate() {
            for (b, tb) in &words[i + 1..] {
                let (a, b) = (a.rect, b.rect);
                if a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom {
                    n += 1;
                    one.get_or_insert_with(|| (ta.to_string(), tb.to_string()));
                }
            }
        }
        (n, one)
    }

    /// Asks for what its parts need drawn ahead: the repos' icons. As a
    /// view lays itself out.
    pub fn want(&self, window: &Window, cx: &mut App) {
        for part in &self.parts {
            if let What::Icon {
                status, side, ends, ..
            } = &part.what
            {
                match ends {
                    Some((a, b)) => {
                        crate::repo_marks::want(status, *a, window, cx);
                        crate::repo_marks::want(status, *b, window, cx);
                    }
                    None => crate::repo_marks::want(status, *side, window, cx),
                }
            }
        }
    }

    /// Paints what lies under the view's own elements: the sheet with its
    /// shadow, and the fills.
    pub fn paint_under(&self, window: &mut Window, cx: &mut App) {
        let s = window.scale_factor();
        if let Some(sheet) = &self.sheet {
            let frame = paint::bounds(sheet.frame, (0, 0), s);
            let radii = gpui::Corners::all(px(sheet.radius));
            if sheet.shadow > 0.0 {
                let shadows = window_shadow().map(|mut shadow| {
                    shadow.color = shadow.color.opacity(shadow.color.a * sheet.shadow);
                    shadow
                });
                window.paint_shadows(frame, radii, &shadows);
            }
            paint::round(window, frame, radii, sheet.bg);
        }
        self.paint_layer(window, cx, false);
    }

    /// Paints what lies over them: the words and pictures, and last the
    /// sheet's rim.
    pub fn paint_over(&self, window: &mut Window, cx: &mut App) {
        let s = window.scale_factor();
        self.paint_layer(window, cx, true);
        if let Some(sheet) = &self.sheet
            && sheet.rim > 0
        {
            window.paint_quad(gpui::quad(
                paint::bounds(sheet.frame, (0, 0), s),
                px(sheet.radius),
                gpui::transparent_black(),
                px(sheet.rim as f32 / s),
                sheet.rim_color,
                gpui::BorderStyle::Solid,
            ));
        }
    }

    fn paint_layer(&self, window: &mut Window, cx: &mut App, over: bool) {
        let s = window.scale_factor();
        let body = paint::bounds(self.body, (0, 0), s);
        for part in self.parts.iter().filter(|p| p.over == over) {
            if part.alpha <= 0.0 {
                continue;
            }
            if part.clip {
                window.with_content_mask(Some(ContentMask { bounds: body }), |window| {
                    paint_part(part, window, cx)
                });
            } else {
                paint_part(part, window, cx);
            }
        }
    }
}

fn paint_part(part: &Part, window: &mut Window, cx: &mut App) {
    let s = window.scale_factor();
    let r = part.rect;
    let b = paint::bounds(r, (0, 0), s);
    let faded = |c: Hsla| {
        if part.alpha >= 1.0 {
            c
        } else {
            c.opacity(c.a * part.alpha)
        }
    };
    match &part.what {
        What::Fill { color, radii } => {
            if radii.iter().all(|r| *r <= 0.0) {
                paint::fill(window, b, faded(*color));
            } else {
                let radii = gpui::Corners {
                    top_left: px(radii[0]),
                    top_right: px(radii[1]),
                    bottom_right: px(radii[2]),
                    bottom_left: px(radii[3]),
                };
                paint::round(window, b, radii, faded(*color));
            }
        }
        What::Words { text, look, align } => {
            let look = Look {
                color: faded(look.color),
                ..*look
            };
            // Part way, the words' size and their box's width are each
            // rounded on their own: a px short is no reason to cut them.
            // They stay placed across their box all the same (centred
            // words don't jump aside as they set off).
            let slack = (2.0 * s).ceil() as i32;
            let wide = (text::width(text, &look, window) * s).ceil() as i32;
            let room = if part.moving && wide <= r.width() + slack {
                r.width().max(wide)
            } else {
                r.width()
            };
            text::paint(
                window,
                text,
                &look,
                (r.left, r.top),
                line_height(look.size),
                Some(room),
                *align,
            );
        }
        What::Health {
            counts,
            colors,
            track,
            checking,
        } => paint::health(
            window,
            r,
            (0, 0),
            *counts,
            colors.map(faded),
            faded(*track),
            *checking,
        ),
        What::Icon {
            status,
            side,
            dot,
            ends,
        } => match ends {
            None => crate::repo_marks::paint(status, b, *side, *dot, part.alpha, window, cx),
            Some((from, to)) => icon_between(status, r, *side, (*from, *to), *dot, window, cx),
        },
        What::AppIcon => paint::app_icon(window, r, (0, 0), part.alpha),
    }
}

/// A repo's icon part way between two sizes: the picture of the nearer
/// of them, drawn `side` logical px square on whole device px.
fn icon_between(
    status: &RepoStatus,
    r: Rect,
    side: f32,
    (from, to): (f32, f32),
    dot: Hsla,
    window: &mut Window,
    cx: &App,
) {
    let s = window.scale_factor();
    let n = |v: f32| (v * s).round().max(1.0) as u32;
    let (near, far) = if (side - from).abs() <= (side - to).abs() {
        (from, to)
    } else {
        (to, from)
    };
    let sides = [n(near), n(far)];
    let shown = n(side);
    let at: Bounds<Pixels> = Bounds::new(
        point(px(r.left as f32 / s), px(r.top as f32 / s)),
        size(px(shown as f32 / s), px(shown as f32 / s)),
    );
    if let Some(image) = crate::repo_marks::picture(status, &sides, cx) {
        window
            .paint_image(at, Default::default(), image, 0, false)
            .ok();
    } else if crate::repo_marks::shows_dot(status, &sides, cx) {
        crate::repo_marks::paint_dot(at, side, dot, window);
    }
    if status.off_trunk() {
        crate::repo_marks::paint_branch_badge(at, side, 1.0, window);
    }
}

/// The shadow Windows 11 draws around an active window, logical px:
/// fitted to its own, measured around the panel and the big window, an
/// even one all round and a deep one below. Drawn around the panel and,
/// in the flip, the big window's sheet, so they hand over to Windows' own
/// unseen.
pub fn window_shadow() -> [gpui::BoxShadow; 2] {
    [
        gpui::BoxShadow {
            color: gpui::black().opacity(0.33),
            offset: point(px(0.0), px(-0.5)),
            blur_radius: px(12.4),
            spread_radius: px(4.4),
        },
        gpui::BoxShadow {
            color: gpui::black().opacity(0.36),
            offset: point(px(0.0), px(45.0)),
            blur_radius: px(20.2),
            spread_radius: px(0.4),
        },
    ]
}
