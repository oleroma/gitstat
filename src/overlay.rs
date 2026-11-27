//! The always-on-top corner badge with the unsynced count, and the
//! floating view it grows into on a right click: a row per repo, its tree
//! and its icon.
//!
//! The badge is a small sheet with its count on it; the view is that
//! sheet grown, with the same corners, away from the screen's corner the
//! badge is kept by. Growing, the count fades, then the sheet grows, then
//! the rows fade in; shrinking is the same run backwards. All of it is
//! drawn inside a window that stays put, as big as the view: nothing is
//! moved or resized to animate.
//!
//! The window is always just as big as what it has to hold (the badge,
//! or the view once it grows) plus a little transparent room. When that
//! changes, a new window comes up cloaked, draws the same picture in the
//! same place, and takes the old one's place in one step, as the flip's
//! windows do: resizing a window would show a frame of it stretched or
//! empty.
//!
//! A window is opened hidden (`show: false`) and never activated: GPUI's
//! first `activate()` would apply the placement parked at creation. It is
//! placed, shown, hidden and moved natively from spawned tasks, so GPUI's
//! WndProc callbacks never meet a borrowed App.
//!
//! Clicks outside the sheet go to the window underneath through a window
//! region. A region is the only per-pixel hit test that works across
//! processes here: `WS_EX_TRANSPARENT` passes every click, `HTTRANSPARENT`
//! from `WM_NCHITTEST` only reaches windows of the same thread, and a
//! layered window's alpha hit-testing needs `UpdateLayeredWindow`, which
//! GPUI's DirectComposition swap chain doesn't use. The region is 1-bit and
//! so jagged; it is a few px larger than the sheet, which GPUI draws
//! anti-aliased with a soft shadow entirely inside it.
//!
//! Dragging with the left button moves the badge, or the view by its top
//! bar; a press that moves less than 4 px is a click. GPUI captures the
//! mouse on button-down (`SetCapture` in its `WM_LBUTTONDOWN` handler,
//! released on button-up), so moves and the release arrive while the
//! cursor is outside the small window. Positions come from the screen
//! cursor, not GPUI's window-relative ones, which shift as the window
//! moves. The window is moved with `SetWindowPos` from spawned tasks, never
//! activated. On release the position is saved as the nearest corner plus
//! offsets.

use crate::config::Corner;
use crate::git::RepoStatus;
use crate::icon::{self, Badge};
use crate::layout::{self, Drag, Rect};
use crate::panel::{self, Anchor};
use crate::scene::float as place;
use crate::scene::paint;
use crate::scene::panel::{TINY, line_height};
use crate::scene::parts::{Key, Part, Picture, Sheet, What, lerp_color};
use crate::scene::text::Look;
use crate::state::{AppState, FloatLists, Input};
use crate::timeline::Timelines;
use crate::view::{self, Palette, Ui};
use crate::win;
use gpui::{
    AnyElement, App, AppContext, Bounds, BoxShadow, ContentMask, Context, DispatchPhase, Element,
    ElementId, Entity, Global, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render, ScrollWheelEvent,
    SharedString, Style, Window, WindowAppearance, WindowBackgroundAppearance, WindowBounds,
    WindowHandle, WindowKind, WindowOptions, point, px, relative, rgb, size,
};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the badge rechecks the work area, fullscreen apps, its
/// z-order and the keepalive, while it is enabled.
const TICK: Duration = Duration::from_secs(2);
/// How long the badge takes to grow into the view, or back.
const GROW: Duration = Duration::from_millis(280);
/// Of that: how long the badge's count takes to fade, and how long the
/// view's rows take. The sheet grows over the rest, between the two.
const FACE: f32 = 0.18;
const ROWS: f32 = 0.24;
/// How fast a row comes to its place, as the time it takes to cover
/// most of the way, seconds.
const SETTLE: f32 = 0.045;
/// How many changes to the rows can be undone.
const UNDO_STEPS: usize = 20;

pub struct OverlayView {
    state: Entity<AppState>,
    timelines: Option<Entity<Timelines>>,
}

impl OverlayView {
    fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        // The view's trees, and its repos' icons as they are drawn.
        let timelines = cx.try_global::<Ui>().map(|ui| ui.timelines.clone());
        if let Some(t) = &timelines {
            cx.observe(t, |_, _, cx| cx.notify()).detach();
        }
        if let Some(marks) = cx.try_global::<Ui>().map(|ui| ui.marks.clone()) {
            cx.observe(&marks, |_, _, cx| cx.notify()).detach();
        }
        Self { state, timelines }
    }
}

/// The badge's fill and its text for a badge state.
fn face(badge: Badge) -> (Hsla, String) {
    match badge {
        Badge::Checking => (rgb(0x6e7681).into(), "…".to_string()),
        Badge::Problem => (rgb(0xd29922).into(), "!".to_string()),
        Badge::AllSynced => (rgb(0x2ea043).into(), "✓".to_string()),
        Badge::Unsynced(n) => (rgb(0xd73a49).into(), icon::count_text(n)),
        Badge::Count(n) => (rgb(0x2ea043).into(), icon::count_text(n)),
    }
}

/// Text height as a share of the badge's side, smaller for longer counts.
fn text_share(text: &str) -> f32 {
    match text.chars().count() {
        0 | 1 => 0.45,
        2 => 0.38,
        _ => 0.30,
    }
}

/// How far each of the three things that change is, `g` of the way from
/// the badge to the view. Never two at once: nothing fades while the
/// sheet moves.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Pose {
    /// The badge's count, 1 on the badge.
    face: f32,
    /// The sheet, 0 the badge's and 1 the view's.
    sheet: f32,
    /// The view's rows, 1 in the view.
    rows: f32,
}

fn pose(g: f32) -> Pose {
    let g = g.clamp(0.0, 1.0);
    let k = ((g - FACE) / (1.0 - FACE - ROWS)).clamp(0.0, 1.0);
    Pose {
        face: 1.0 - (g / FACE).min(1.0),
        // Eased the same at both ends: shrinking is growing run backwards.
        sheet: k * k * (3.0 - 2.0 * k),
        rows: ((g - (1.0 - ROWS)) / ROWS).clamp(0.0, 1.0),
    }
}

/// What of the view the mouse is on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Spot {
    /// Not on it.
    #[default]
    Off,
    /// The sheet itself: where the mouse takes hold of it.
    Sheet,
    /// A row's tree, which the view is dragged by too, and its icon.
    Strip(usize),
    Icon(usize),
    /// The undo button on its plate beside the view.
    Undo,
    /// The side edge that is dragged to make the view wider.
    Edge,
    /// The pull requests' marks.
    Tail,
}

/// A row taken hold of by its icon, on its way to another place.
struct Reorder {
    path: PathBuf,
    from: usize,
    /// Where it would land now.
    to: usize,
    /// The cursor's y when it was taken, and how far it has gone since.
    start: i32,
    dy: i32,
}

/// Where the mouse finds what, in the frame drawn last: device px from
/// the window's corner.
#[derive(Default)]
struct Spots {
    /// The rows are in: before that, and as the badge, there is only the
    /// sheet.
    live: bool,
    sheet: Rect,
    edge: Option<Rect>,
    undo: Option<Rect>,
    list: Option<Rect>,
    /// Each row and its icon's column, cut to the list.
    rows: Vec<(Rect, Rect)>,
    /// The first row's tree, cut to the list: for the self-test.
    first_strip: Option<Rect>,
    tail: Option<Rect>,
}

impl Spots {
    fn at(&self, at: (i32, i32)) -> Spot {
        if self.undo.is_some_and(|r| contains(r, at)) {
            return Spot::Undo;
        }
        if !self.live {
            return if contains(self.sheet, at) {
                Spot::Sheet
            } else {
                Spot::Off
            };
        }
        if self.edge.is_some_and(|r| contains(r, at)) {
            return Spot::Edge;
        }
        if !contains(self.sheet, at) {
            return Spot::Off;
        }
        if self.tail.is_some_and(|r| contains(r, at)) {
            return Spot::Tail;
        }
        if self.list.is_some_and(|l| contains(l, at)) {
            for (i, (rect, label)) in self.rows.iter().enumerate() {
                if contains(*label, at) {
                    return Spot::Icon(i);
                }
                if contains(*rect, at) {
                    return Spot::Strip(i);
                }
            }
        }
        Spot::Sheet
    }
}

/// The undo button's glyph, in the title bar's font, and its size,
/// logical px.
const GLYPH_UNDO: &str = "\u{e7a7}";
const GLYPH: f32 = 12.0;
/// How long the undo plate stays after the last change, and how long it
/// takes to come and go.
const PLATE_STAYS: Duration = Duration::from_secs(8);
const PLATE_FADE: f32 = 0.15;

/// A repo's row in the view.
struct RowPart {
    repo: Arc<RepoStatus>,
    color: Hsla,
    /// Its tree, and where the mouse finds its icon.
    graph: Option<AnyElement>,
    label: Option<AnyElement>,
    /// How far from its place it is drawn, device px down: on its way to
    /// another, or making room for the one that is.
    offset: i32,
}

/// The badge, the view, or the sheet part way between them, as one
/// element as big as its window.
struct Scene {
    hwnd: Option<isize>,
    p: Palette,
    fill: Hsla,
    text: SharedString,
    pose: Pose,
    moving: bool,
    /// The badge and the view in this window, device px from its corner.
    badge: Rect,
    frame: Rect,
    /// The corners' radius, logical px.
    radius: f32,
    opacity: f32,
    health: Option<([usize; 3], bool)>,
    rows: Vec<RowPart>,
    scroll: i32,
    /// The undo plate, in this window, and how far it has come.
    plate: Option<(Rect, f32)>,
    /// The row taken hold of.
    dragged: Option<usize>,
    spot: Spot,
    font: &'static str,
    /// The open pull requests' marks.
    prs: Vec<Hsla>,
    /// The view's free side, the one dragged to size it, is its left.
    free_left: bool,
}

struct Laid {
    sheet: Rect,
    picture: Picture,
    slots: Vec<AnyElement>,
    list: Option<Rect>,
    spots: Spots,
    /// Where the mouse shows that the view can be made wider.
    edge: Option<gpui::Hitbox>,
    /// The undo plate, drawn with its own shadow.
    plate: Option<(Rect, f32)>,
    max_scroll: i32,
    strip: i32,
}

fn contains(r: Rect, (x, y): (i32, i32)) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

/// A glyph of the title bar's font, in the middle of `r`.
fn glyph(r: Rect, glyph: &'static str, color: Hsla, font: &'static str, s: f32) -> Part {
    let lh = place::px(line_height(GLYPH), s);
    let top = r.top + (r.height() - lh) / 2;
    Part::words(
        Key::Own,
        Rect::new(r.left, top, r.right, top + lh),
        glyph,
        Look::new(GLYPH, color).family(font),
    )
    .centred()
}

impl Scene {
    fn lay(&mut self, window: &mut Window, cx: &mut App) -> Laid {
        let s = window.scale_factor();
        let p = self.p;
        let k = self.pose.sheet;
        let (b, f) = (self.badge, self.frame);
        let mid = |a: i32, b: i32| (a as f32 + (b - a) as f32 * k).round() as i32;
        let sheet = if k <= 0.0 {
            b
        } else if k >= 1.0 {
            f
        } else {
            Rect::new(
                mid(b.left, f.left),
                mid(b.top, f.top),
                mid(b.right, f.right),
                mid(b.bottom, f.bottom),
            )
        };
        let ring = Hsla {
            l: self.fill.l * 0.75,
            ..self.fill
        };
        let mut parts = Vec::new();
        if self.pose.face > 0.0 {
            // A whole number of device px: DirectWrite hints whole sizes
            // crisply, and a fractional one blurs every stem.
            let side = b.width().min(b.height()) as f32;
            let text_size = (side * text_share(&self.text)).round() / s;
            let lh = place::px(line_height(text_size), s);
            let top = b.top + (b.height() - lh) / 2;
            let mut count = Part::words(
                Key::Own,
                Rect::new(b.left, top, b.right, top + lh),
                self.text.clone(),
                Look {
                    weight: 700.0,
                    ..Look::new(text_size, gpui::white())
                },
            )
            .centred();
            count.alpha = self.pose.face;
            parts.push(count);
        }

        let mut slots = Vec::new();
        let mut list = None;
        let mut spots = Spots {
            sheet,
            ..Default::default()
        };
        let mut edge = None;
        let mut max_scroll = 0;
        if self.pose.rows > 0.0 {
            let alpha = self.pose.rows;
            let at = (f.left, f.top);
            let l = place::layout(&place::Spec {
                scale: s,
                badge: b.width(),
                width: f.width(),
                max_height: f.height(),
                radius: self.radius,
                rows: self.rows.len(),
                health: self.health.is_some(),
                buttons: 0,
                tail: usize::from(!self.prs.is_empty()),
                scroll: self.scroll,
            });
            // Scrolled past its end (rows went): back to it.
            if l.scroll != self.scroll {
                cx.default_global::<OverlaySlot>().scroll = l.scroll;
            }
            max_scroll = l.max_scroll;
            let sh =
                |r: Rect| Rect::new(r.left + at.0, r.top + at.1, r.right + at.0, r.bottom + at.1);
            let down = |r: Rect, d: i32| Rect::new(r.left, r.top + d, r.right, r.bottom + d);
            let faded = |mut part: Part| {
                part.alpha = alpha;
                part
            };
            let hover = p.hover;
            // What the mouse is on shows it.
            let lit = |r: Rect| faded(Part::fill(Key::Own, r, hover).radii([4.0; 4]));
            if let (Some(bar), Some((counts, checking))) = (l.bar, self.health) {
                parts.push(faded(Part::new(
                    Key::Health,
                    sh(bar),
                    What::Health {
                        counts,
                        colors: [p.green, p.amber, p.red],
                        track: p.hover,
                        checking,
                    },
                )));
            }
            if let Some(empty) = l.empty {
                let lh = place::px(line_height(TINY), s);
                let top = empty.top + (empty.height() - lh) / 2;
                parts.push(faded(
                    Part::words(
                        Key::Own,
                        sh(Rect::new(empty.left, top, empty.right, top + lh)),
                        "Pin repos from the panel's menu",
                        Look::new(TINY, p.muted),
                    )
                    .centred(),
                ));
            }
            let in_sight = |r: Rect| r.bottom > l.list.top && r.top < l.list.bottom;
            // The row taken hold of is drawn last, over the others.
            let mut order: Vec<usize> = (0..self.rows.len().min(l.rows.len())).collect();
            if let Some(d) = self.dragged.filter(|d| *d < order.len()) {
                order.remove(d);
                order.push(d);
            }
            for &i in &order {
                let (part, row) = (&self.rows[i], &l.rows[i]);
                let d = part.offset;
                if !in_sight(down(row.rect, d)) {
                    continue;
                }
                if self.dragged == Some(i) {
                    parts.push(lit(sh(down(row.rect, d))).clipped());
                }
                parts.push(faded(
                    Part::new(
                        Key::Icon(part.repo.path.clone()),
                        sh(down(row.icon, d)),
                        What::Icon {
                            status: part.repo.clone(),
                            side: place::MARK,
                            dot: part.color,
                            ends: None,
                        },
                    )
                    .clipped(),
                ));
            }
            if let Some(tail) = l.tail.first() {
                let (side, step) = (place::px(place::PR_MARK, s), place::px(place::PR_STEP, s));
                let top = tail.top + (tail.height() - side) / 2;
                for (i, color) in self.prs.iter().enumerate() {
                    let left = tail.left + i as i32 * step;
                    if left + side > tail.right {
                        break;
                    }
                    parts.push(faded(
                        Part::fill(
                            Key::Own,
                            sh(Rect::new(left, top, left + side, top + side)),
                            *color,
                        )
                        .radii([place::PR_MARK / 2.0; 4]),
                    ));
                }
                spots.tail = Some(sh(*tail));
            }
            let list_bounds = paint::bounds(l.list, at, s);
            window.with_content_mask(
                Some(ContentMask {
                    bounds: list_bounds,
                }),
                |window| {
                    for &i in &order {
                        let (part, row) = (&mut self.rows[i], &l.rows[i]);
                        let d = part.offset;
                        if !in_sight(down(row.rect, d)) {
                            continue;
                        }
                        if let Some(slot) = part.graph.take() {
                            let r = down(row.strip, d);
                            slots.push(view::put(slot, r, at, s, alpha, window, cx));
                        }
                        if let Some(slot) = part.label.take() {
                            let r = down(row.label, d);
                            slots.push(view::put(slot, r, at, s, alpha, window, cx));
                        }
                    }
                },
            );
            let shown = sh(l.list);
            let cut = |r: Rect| {
                let top = r.top.max(shown.top);
                Rect::new(r.left, top, r.right, r.bottom.min(shown.bottom).max(top))
            };
            spots.rows = l
                .rows
                .iter()
                .map(|row| (cut(sh(row.rect)), cut(sh(row.label))))
                .collect();
            spots.first_strip = l.rows.first().map(|row| cut(sh(row.strip)));
            spots.list = Some(shown);
            spots.live = self.pose.rows >= 1.0;
            list = Some(shown);
            if spots.live {
                // The free side edge, and the clear px beside it: what
                // the view is made wider by.
                let m = place::px(place::inset(self.radius), s);
                let out = place::px(3.0, s);
                let zone = if self.free_left {
                    Rect::new(f.left - out, f.top + m, f.left + m, f.bottom - m)
                } else {
                    Rect::new(f.right - m, f.top + m, f.right + out, f.bottom - m)
                };
                spots.edge = Some(zone);
                edge =
                    Some(window.insert_hitbox(
                        paint::bounds(zone, (0, 0), s),
                        gpui::HitboxBehavior::Normal,
                    ));
            }
        }
        // The undo plate: a small sheet of its own beside the view.
        let plate = self.plate.filter(|(_, a)| *a > 0.0);
        if let Some((r, a)) = plate {
            let rim = (s.round() as i32).max(1);
            let pr = place::PLATE_RADIUS;
            let fade = |mut part: Part| {
                part.alpha = a;
                part
            };
            parts.push(fade(
                Part::fill(Key::Own, r, p.border).radii([pr; 4]).over(),
            ));
            let inner = Rect::new(r.left + rim, r.top + rim, r.right - rim, r.bottom - rim);
            let ground = p.bg.opacity(self.opacity.max(0.85));
            parts.push(fade(
                Part::fill(Key::Own, inner, ground)
                    .radii([pr - rim as f32 / s; 4])
                    .over(),
            ));
            if self.spot == Spot::Undo {
                parts.push(fade(
                    Part::fill(Key::Own, inner, p.hover)
                        .radii([pr - rim as f32 / s; 4])
                        .over(),
                ));
            }
            parts.push(fade(glyph(r, GLYPH_UNDO, p.text, self.font, s)));
            if a >= 1.0 {
                spots.undo = Some(r);
            }
        }
        let picture = Picture {
            sheet: Some(Sheet {
                frame: sheet,
                radius: self.radius,
                rim: (s.round() as i32).max(1),
                rim_color: lerp_color(ring, p.border, k),
                bg: lerp_color(self.fill, p.bg.opacity(self.opacity), k),
                shadow: 0.0,
            }),
            body: list.unwrap_or(sheet),
            parts,
        };
        picture.want(window, cx);
        Laid {
            sheet,
            picture,
            slots,
            list,
            spots,
            edge,
            plate,
            max_scroll,
            strip: place::px(place::STRIP, s),
        }
    }
}

impl IntoElement for Scene {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Scene {
    type RequestLayoutState = ();
    type PrepaintState = Option<Laid>;

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
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        Some(self.lay(window, cx))
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        state: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(laid) = state else {
            return;
        };
        let s = window.scale_factor();
        let dev = move |at: gpui::Point<Pixels>| {
            (
                (f32::from(at.x) * s).floor() as i32,
                (f32::from(at.y) * s).floor() as i32,
            )
        };
        let sheet = laid.sheet;
        let spots = Rc::new(std::mem::take(&mut laid.spots));
        cx.default_global::<OverlaySlot>().first_strip = spots.first_strip;
        let list = laid.list.filter(|_| spots.live);
        let (max_scroll, strip) = (laid.max_scroll, laid.strip);
        if let Some(edge) = &laid.edge {
            window.set_cursor_style(gpui::CursorStyle::ResizeLeftRight, edge);
        }

        // Before the rows paint: their own handlers (a tree's dots, an
        // icon's menu) then come first, and what they don't take lands
        // here. Window-wide, hit or not: with the mouse captured, moves
        // and the release outside the window land here too.
        let at_down = spots.clone();
        window.on_mouse_event(move |e: &MouseDownEvent, phase, _, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let spot = at_down.at(dev(e.position));
            hush(cx);
            match (e.button, spot) {
                (MouseButton::Left, _) => down(spot, cx),
                // An icon: its folder; anywhere else on it, the badge.
                (MouseButton::Right, Spot::Icon(row)) => open_row(row, false, cx),
                (MouseButton::Right, Spot::Off | Spot::Edge | Spot::Undo) => {}
                (MouseButton::Right, _) => toggle(cx),
                (MouseButton::Middle, Spot::Icon(row)) => ask_take_out(row, cx),
                _ => {}
            }
        });
        if !window.is_window_hovered() && cx.default_global::<OverlaySlot>().mouse_in {
            cx.defer(left);
        }
        let at_move = spots.clone();
        window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            moved(e.pressed_button == Some(MouseButton::Left), cx);
            // Over its tooltip or card, the mouse is on them, not on what
            // they cover.
            if e.pressed_button.is_none() && crate::float_tip::holds_mouse(cx) {
                return;
            }
            hover(at_move.at(dev(e.position)), cx);
        });
        let at_up = spots.clone();
        window.on_mouse_event(move |e: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble && e.button == MouseButton::Left {
                up(at_up.at(dev(e.position)), Some(e.click_count), cx);
            }
        });
        if let Some(list) = list.filter(|_| max_scroll > 0) {
            window.on_mouse_event(move |e: &ScrollWheelEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || !contains(list, dev(e.position)) {
                    return;
                }
                // A row at a time: whole rows show.
                let d = f32::from(e.delta.pixel_delta(window.line_height()).y);
                if d == 0.0 {
                    return;
                }
                let slot = cx.default_global::<OverlaySlot>();
                let step = if d < 0.0 { strip } else { -strip };
                let to = (slot.scroll + step).clamp(0, max_scroll);
                if to != slot.scroll {
                    slot.scroll = to;
                    window.refresh();
                    cx.stop_propagation();
                }
            });
        }

        // The sheet with its soft shadow; over it the rows' trees and
        // where the mouse finds their icons; over those the count or the
        // icons, and last the rim.
        let bounds = paint::bounds(sheet, (0, 0), s);
        window.paint_shadows(
            bounds,
            gpui::Corners::all(px(self.radius)),
            &[BoxShadow {
                color: gpui::hsla(0., 0., 0., 0.35),
                offset: point(px(0.), px(0.5)),
                blur_radius: px(0.75),
                spread_radius: px(0.),
            }],
        );
        if let Some((r, a)) = laid.plate {
            window.paint_shadows(
                paint::bounds(r, (0, 0), s),
                gpui::Corners::all(px(place::PLATE_RADIUS)),
                &[BoxShadow {
                    color: gpui::hsla(0., 0., 0., 0.35 * a),
                    offset: point(px(0.), px(0.5)),
                    blur_radius: px(0.75),
                    spread_radius: px(0.),
                }],
            );
        }
        laid.picture.paint_under(window, cx);
        if let Some(list) = laid.list {
            window.with_content_mask(
                Some(ContentMask {
                    bounds: paint::bounds(list, (0, 0), s),
                }),
                |window| {
                    for slot in &mut laid.slots {
                        slot.paint(window, cx);
                    }
                },
            );
        }
        laid.picture.paint_over(window, cx);
        if self.moving {
            window.request_animation_frame();
        }
        drew(self.hwnd, window, cx);
    }
}

impl Render for OverlayView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hwnd = win::hwnd(window);
        let p = Palette::for_appearance(window.appearance());
        let dark = matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let font = view::caption_font(cx);
        let slot = cx.default_global::<OverlaySlot>();
        let (g, moving) = slot.growth();
        let pose = pose(g);
        let scroll = slot.scroll;
        let spot = slot.spot;
        let plate_alpha = slot.plate_alpha();
        let held_lists = slot.lists.clone();
        let held_corner = slot.held.map(|(_, corner)| corner);
        // Where this window is, and so where in it the badge and the view
        // are: nowhere before it is placed.
        let placed = slot
            .pane(hwnd)
            .and_then(|pane| Some((pane.area?, pane.frame, pane.plate)))
            .zip(slot.placed);
        let Some(((area, frame, plate), badge)) = placed else {
            return gpui::div().into_any_element();
        };
        let local = |r: Rect| {
            Rect::new(
                r.left - area.left,
                r.top - area.top,
                r.right - area.left,
                r.bottom - area.top,
            )
        };

        let state = self.state.read(cx);
        let cfg = &state.config;
        let (fill, text) = face(state.badge());
        let (radius, opacity) = (cfg.overlay_radius(), cfg.float_opacity());
        let corner = held_corner.unwrap_or(cfg.overlay.corner);
        let health = cfg
            .float
            .health
            .then(|| (view::tone_counts(&state.statuses), state.sweeping));
        let mut rows = Vec::new();
        let mut tips = Vec::new();
        let mut names = Vec::new();
        let mut prs = Vec::new();
        let mut pr_tip = Lines::new();
        if pose.rows > 0.0 {
            // The trees end on the sheet itself: see-through, they fade
            // into nothing rather than into a colour that isn't there.
            let strip_p = Palette {
                surface: if opacity < 1.0 {
                    p.bg.opacity(0.0)
                } else {
                    p.bg
                },
                ..p
            };
            let lists = held_lists.unwrap_or_else(|| FloatLists::of(cfg));
            for (i, s) in state
                .float_repos_of(&lists, unix_now())
                .into_iter()
                .enumerate()
            {
                let color = view::status_color(s, &p);
                let (graph, label, checkout, status) = match &self.timelines {
                    Some(t) => {
                        let strip = Timelines::strip(t, i, s, &strip_p, dark, true, true, cx);
                        (
                            Some(strip.graph),
                            Some(strip.label),
                            strip.checkout,
                            strip.status,
                        )
                    }
                    None => (None, None, String::new(), s.label()),
                };
                let mut tip = vec![(s.name.clone(), p.text)];
                if !checkout.is_empty() {
                    tip.push((checkout, p.muted));
                }
                tip.push((status, color));
                tip.extend(
                    view::status_details(s, &p)
                        .into_iter()
                        .map(|(l, c)| (l.to_string(), c)),
                );
                tips.push(tip);
                names.push((s.path.clone(), AppState::listed_name(s)));
                rows.push(RowPart {
                    repo: Arc::new(s.clone()),
                    color,
                    graph,
                    label,
                    offset: 0,
                });
            }
            if show_prs(cfg) {
                for (repo, pr) in state.open_prs() {
                    let (color, words) = view::pr_tone(pr, &p);
                    prs.push(color);
                    let mut line = format!("{} #{}: {}", repo.name, pr.number, pr.title);
                    if !words.is_empty() {
                        line.push_str(&format!(" ({words})"));
                    }
                    pr_tip.push((line, color));
                }
            }
        }
        let strip = place::px(place::STRIP, window.scale_factor());
        let slot = cx.default_global::<OverlaySlot>();
        let paths: Vec<&Path> = names.iter().map(|(path, _)| path.as_path()).collect();
        let rows_moving = slot.settle(&paths, strip);
        let plate_moving = slot.plate_fading();
        for (row, (path, _)) in rows.iter_mut().zip(&names) {
            row.offset = slot.offset(path);
        }
        let dragged = slot
            .reorder
            .as_ref()
            .and_then(|r| names.iter().position(|(path, _)| *path == r.path));
        slot.tips = tips;
        slot.names = names;
        slot.pr_tip = pr_tip;
        slot.ink = (p.text, p.muted);
        slot.strip = strip;
        Scene {
            hwnd,
            p,
            fill,
            text: text.into(),
            pose,
            moving: moving || rows_moving || plate_moving,
            badge: local(badge),
            frame: local(frame),
            radius,
            opacity,
            health,
            rows,
            scroll,
            plate: plate.map(|r| (local(r), plate_alpha)),
            dragged,
            spot,
            font,
            prs,
            free_left: matches!(corner, Corner::TopRight | Corner::BottomRight),
        }
        .into_any_element()
    }
}

/// Whether the view shows the open pull requests' marks.
fn show_prs(cfg: &crate::config::Config) -> bool {
    cfg.float.prs && cfg.github_prs
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// A window of the badge's: the one on the screen, or the one coming up
/// to take its place.
struct Pane {
    handle: WindowHandle<OverlayView>,
    hwnd: isize,
    /// Its inside on the screen, physical px, once it is placed.
    area: Option<Rect>,
    /// The view's sheet on the screen, as this window holds it, and the
    /// undo plate beside it.
    frame: Rect,
    plate: Option<Rect>,
    /// How it is cut, as last asked: see `win::shape_overlay`.
    shape: Option<Option<Vec<(Rect, i32)>>>,
    /// Frames it has drawn where it is placed.
    drawn: u32,
}

#[derive(Default)]
pub struct OverlaySlot {
    cur: Option<Pane>,
    /// Coming up cloaked, to take `cur`'s place once it has drawn.
    next: Option<Pane>,
    /// Where the badge was last put, in physical px. Its window is this
    /// (or, grown, the view) grown by `pad`.
    placed: Option<Rect>,
    /// Transparent physical px around the sheet, inside the window region.
    pad: i32,
    visible: bool,
    /// A left-button press on the badge, until its release.
    drag: Option<Drag>,
    /// Where the user put the badge, and the corner it is kept by there,
    /// while dragging and until the config holds it: placement uses this
    /// over the config's corner and offset.
    held: Option<(Rect, Corner)>,
    /// The next window position for the move task; `Some` while one is
    /// queued.
    move_to: Rc<Cell<Option<(i32, i32)>>>,
    /// The running timer's id; a timer whose id is no longer here stops.
    timer: Option<u64>,
    timers_started: u64,
    /// The tooltip asked for last, and where it shows once it does: drawn
    /// by `float_tip`, as this window is no bigger than what it shows.
    tip_text: Lines,
    tip_at: Option<(i32, i32)>,
    /// Counts the tooltips asked for: one whose wait is over shows only
    /// if no other was asked for meanwhile.
    tip_asked: u64,
    /// A press hushed the tooltip, until the mouse goes somewhere else.
    tip_hushed: bool,
    /// What the tooltip asked for last is of.
    tip_spot: Spot,
    /// The mouse is over the window.
    mouse_in: bool,
    /// Whether it is the view (or on its way to it); none before the
    /// first sync.
    grown: Option<bool>,
    /// On its way there: how far grown it set off, and when. No time yet
    /// while the window that holds the view is still coming up.
    anim: Option<(f32, Option<Instant>)>,
    /// How far the view's rows are scrolled, physical px.
    scroll: i32,
    /// What of the view the mouse is on, and the button it went down on.
    spot: Spot,
    armed: Option<Spot>,
    /// What each row says under the mouse, the pull requests' marks, and
    /// the view as a whole.
    tips: Vec<Lines>,
    pr_tip: Lines,
    summary: Lines,
    /// The colours of a tooltip's words and of its hints.
    ink: (Hsla, Hsla),
    /// The rows as drawn last: each repo's path and the name it is
    /// listed by; and a row's height, device px.
    names: Vec<(PathBuf, String)>,
    strip: i32,
    /// The lists before each change made here, the latest last.
    undo: Vec<FloatLists>,
    /// The plate that holds the undo button, while there is one.
    plate: Option<Plate>,
    /// A left press on a row's icon, and where the cursor was: a click
    /// opens the repo, a drag takes the row to another place.
    icon_press: Option<(usize, (i32, i32))>,
    /// The press on the view under way is a click that opens the panel
    /// (on the sheet), or none (on a tree).
    press_opens: bool,
    /// The lists as changed here, until the config holds them.
    lists: Option<FloatLists>,
    /// A row on its way to another place.
    reorder: Option<Reorder>,
    /// How far from its place each row is drawn, device px down, for the
    /// ones that are; and when that was last worked out.
    offsets: Vec<(PathBuf, f32)>,
    offsets_at: Option<Instant>,
    /// The view's width as dragged, device px, until the config holds
    /// it; and whether the drag is still on.
    width: Option<(i32, bool)>,
    /// That drag: the cursor's x and the width when it began.
    resize: Option<(i32, i32)>,
    /// The window the press of that drag landed in, kept (unseen) while
    /// the drag lasts: it has the mouse.
    captor: Option<Pane>,
    /// The first row's tree in `cur`, client px: for the self-test.
    first_strip: Option<Rect>,
}

impl Global for OverlaySlot {}

/// A tooltip's lines, each in its colour.
type Lines = Vec<(String, Hsla)>;

/// The undo plate's coming and going.
#[derive(Clone, Copy, Debug)]
struct Plate {
    /// When it began to show: none while the window that holds it is
    /// still coming up.
    shown: Option<Instant>,
    /// When it goes.
    until: Instant,
}

impl OverlaySlot {
    /// How far the undo plate has come, 0 to 1.
    fn plate_alpha(&self) -> f32 {
        let Some(plate) = self.plate else { return 0.0 };
        let Some(shown) = plate.shown else { return 0.0 };
        let now = Instant::now();
        let came = (now.duration_since(shown).as_secs_f32() / PLATE_FADE).min(1.0);
        let left = if now >= plate.until {
            1.0 - (now.duration_since(plate.until).as_secs_f32() / PLATE_FADE).min(1.0)
        } else {
            1.0
        };
        came.min(left)
    }

    /// The plate is coming or going: drawn every frame.
    fn plate_fading(&self) -> bool {
        let Some(Plate {
            shown: Some(shown),
            until,
        }) = self.plate
        else {
            return false;
        };
        let now = Instant::now();
        now.duration_since(shown).as_secs_f32() < PLATE_FADE || now >= until
    }

    /// The plate has faded away.
    fn plate_gone(&self) -> bool {
        self.plate
            .is_some_and(|p| p.shown.is_some() && Instant::now() >= p.until)
            && self.plate_alpha() <= 0.0
    }

    /// How far grown it is now, 0 the badge and 1 the view, and whether
    /// it is on its way.
    fn growth(&self) -> (f32, bool) {
        let target = if self.grown == Some(true) { 1.0 } else { 0.0 };
        match self.anim {
            None => (target, false),
            Some((from, None)) => (from, false),
            Some((from, Some(start))) => {
                let span = (target - from).abs();
                if span <= 0.0 {
                    return (target, false);
                }
                let len = GROW.as_secs_f32() * span;
                let e = (start.elapsed().as_secs_f32() / len).min(1.0);
                (from + (target - from) * e, e < 1.0)
            }
        }
    }

    fn pane(&self, hwnd: Option<isize>) -> Option<&Pane> {
        let hwnd = hwnd?;
        [&self.cur, &self.next, &self.captor]
            .into_iter()
            .flatten()
            .find(|pane| pane.hwnd == hwnd)
    }

    /// How far from its place the row of `path` is drawn.
    fn offset(&self, path: &Path) -> i32 {
        self.offsets
            .iter()
            .find(|(p, _)| p == path)
            .map_or(0, |(_, d)| d.round() as i32)
    }

    /// Moves each row a step nearer to where it belongs: its own place,
    /// or out of the way of the row taken hold of, which goes where the
    /// mouse has it. Whether any is still on its way.
    fn settle(&mut self, rows: &[&Path], strip: i32) -> bool {
        let now = Instant::now();
        let dt = self
            .offsets_at
            .replace(now)
            .map_or(0.0, |t| now.duration_since(t).as_secs_f32())
            .min(0.05);
        let near = 1.0 - (-dt / SETTLE).exp();
        let mut next = Vec::new();
        let mut moving = false;
        for (i, path) in rows.iter().enumerate() {
            let mut d = self
                .offsets
                .iter()
                .find(|(p, _)| p == path)
                .map_or(0.0, |(_, d)| *d);
            let target = match &self.reorder {
                Some(r) if r.path == *path => {
                    d = r.dy as f32;
                    d
                }
                Some(r) if r.from < i && i <= r.to => -strip as f32,
                Some(r) if r.to <= i && i < r.from => strip as f32,
                _ => 0.0,
            };
            d += (target - d) * near;
            if (target - d).abs() < 0.5 {
                d = target;
            } else {
                moving = true;
            }
            if d != 0.0 {
                next.push((path.to_path_buf(), d));
            }
        }
        self.offsets = next;
        if !moving {
            self.offsets_at = None;
        }
        moving
    }

    /// What shows on the screen now: the view once grown, else the badge.
    fn shape(&self) -> Option<Rect> {
        let cur = self.cur.as_ref()?;
        if self.growth().0 >= 1.0 && cur.area.is_some() {
            Some(cur.frame)
        } else {
            self.placed
        }
    }
}

/// How far grown the badge was in the frame drawn last, in thousandths:
/// for the self-test, which reads it beside each picture it takes of the
/// screen.
static DRAWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(u32::MAX);

/// How far grown the frame drawn last is, in thousandths, if any was.
pub fn drawn() -> Option<(u16, u16, u16)> {
    let g = DRAWN.load(std::sync::atomic::Ordering::Relaxed);
    u16::try_from(g).ok().map(|g| (g, 0, 0))
}

/// At rest: whether it is the view, once nothing grows, shrinks or
/// changes windows any more.
pub fn resting(cx: &App) -> Option<bool> {
    let slot = cx.try_global::<OverlaySlot>()?;
    let cur = slot.cur.as_ref()?;
    (slot.anim.is_none() && slot.next.is_none() && cur.area.is_some() && slot.visible)
        .then_some(slot.grown == Some(true))
}

/// A window of the badge's drew a frame: one coming up counts them until
/// it can take over, and a growth that has arrived ends.
fn drew(hwnd: Option<isize>, window: &mut Window, cx: &mut App) {
    let s = window.scale_factor();
    let viewport = window.viewport_size();
    let size = (
        (f32::from(viewport.width) * s).round() as i32,
        (f32::from(viewport.height) * s).round() as i32,
    );
    let slot = cx.default_global::<OverlaySlot>();
    let (g, moving) = slot.growth();
    DRAWN.store(
        (g * 1000.0).round() as u32,
        std::sync::atomic::Ordering::Relaxed,
    );
    let arrived = matches!(slot.anim, Some((_, Some(_)))) && !moving;
    let faded = slot.plate_gone();
    let mut swap = false;
    if let Some(next) = slot.next.as_mut().filter(|n| Some(n.hwnd) == hwnd) {
        // Only frames drawn where it belongs, at the size it is to have.
        if next.area.is_some_and(|a| {
            (a.width(), a.height()) == size && win::client_rect(next.hwnd) == Some(a)
        }) {
            next.drawn += 1;
        }
        // Twice: the second is surely of that place.
        swap = next.drawn >= 2;
        if !swap {
            window.request_animation_frame();
        }
    }
    if swap {
        cx.defer(finish_swap);
    }
    if faded {
        cx.defer(sync);
    }
    if arrived {
        cx.defer(|cx| {
            let slot = cx.default_global::<OverlaySlot>();
            if matches!(slot.anim, Some((_, Some(_)))) && !slot.growth().1 {
                slot.anim = None;
            }
            sync(cx);
        });
    }
}

/// The window coming up has drawn what the one on the screen shows, in
/// the same place: it takes over in one step, between two frames.
fn finish_swap(cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    let Some(next) = slot.next.take() else {
        return;
    };
    let new = next.hwnd;
    let old = slot.cur.replace(next);
    win::dwm_flush();
    win::uncloak(new);
    // What the view says stays over it.
    if let Some(tip) = crate::float_tip::shown_hwnd(cx) {
        cx.spawn(async move |_| unsafe { win::reassert_topmost(tip) })
            .detach();
    }
    if let Some(old) = old {
        win::cloak(old.hwnd);
        let slot = cx.default_global::<OverlaySlot>();
        if slot.resize.is_some() && slot.captor.is_none() {
            // The press of a drag landed in it: it has the mouse until
            // the release, so it stays, unseen.
            slot.captor = Some(old);
        } else {
            view::remove(old.handle.into(), cx);
        }
    }
    sync(cx);
}

/// Redraws the badge's windows.
fn redraw(cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    let handles: Vec<_> = [&slot.cur, &slot.next]
        .into_iter()
        .flatten()
        .map(|pane| pane.handle)
        .collect();
    // Not from inside one of their own mouse events, where the window
    // is out of reach.
    cx.defer(move |cx| {
        for handle in handles {
            handle.update(cx, |_, window, _| window.refresh()).ok();
        }
    });
}

/// A right click on the badge or the view: the one becomes the other.
fn toggle(cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    if slot.drag.is_some() || slot.resize.is_some() || slot.reorder.is_some() {
        return;
    }
    let grown = slot.grown == Some(true);
    if grown {
        // Shrinking ends whatever was being done to the rows.
        slot.reorder = None;
        slot.armed = None;
        slot.icon_press = None;
        // and any card: the dot under the mouse goes.
        let own = slot.cur.as_ref().map(|cur| cur.hwnd);
        if let Some(t) = cx.try_global::<Ui>().map(|ui| ui.timelines.clone()) {
            t.update(cx, |t, cx| t.drop_hover_in(own, cx));
        }
    }
    if let Some(ui) = cx.try_global::<Ui>() {
        ui.input.unbounded_send(Input::OverlayGrown(!grown)).ok();
    }
}

/// The mouse moved: what it rests on says what it is, and shows it.
fn hover(spot: Spot, cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    slot.mouse_in = true;
    if slot.spot == spot {
        return;
    }
    slot.spot = spot;
    slot.tip_hushed = false;
    let text = tip_for(slot);
    sync_tooltip(text, cx);
    redraw(cx);
}

/// The mouse left the window: nothing of it is hovered any more, and
/// what was said of it goes.
pub fn left(cx: &mut App) {
    if !cx.default_global::<OverlaySlot>().mouse_in {
        return;
    }
    // Onto its tooltip or card: they stay while the mouse is on them.
    if crate::float_tip::holds_mouse(cx) {
        crate::float_tip::watch_mouse(cx);
        return;
    }
    let slot = cx.default_global::<OverlaySlot>();
    slot.mouse_in = false;
    slot.spot = Spot::Off;
    let own = slot.cur.as_ref().map(|cur| cur.hwnd);
    sync_tooltip(Vec::new(), cx);
    if let Some(t) = cx.try_global::<Ui>().map(|ui| ui.timelines.clone()) {
        t.update(cx, |t, cx| t.drop_hover_in(own, cx));
    }
    redraw(cx);
}

/// A press: the tooltip goes, until the mouse moves on to something else.
fn hush(cx: &mut App) {
    cx.default_global::<OverlaySlot>().tip_hushed = true;
    sync_tooltip(Vec::new(), cx);
}

/// What the tooltip says now: what the mouse is on, else, grown, the
/// tray's summary. A tree says nothing: its dots have their cards.
fn tip_for(slot: &OverlaySlot) -> Lines {
    if slot.growth().0 < 1.0
        || slot.reorder.is_some()
        || slot.resize.is_some()
        || slot.tip_hushed
        || !slot.mouse_in
    {
        return Lines::new();
    }
    let hint = |t: &str| (t.to_string(), slot.ink.1);
    match slot.spot {
        Spot::Icon(row) => match slot.tips.get(row) {
            Some(tip) => {
                let mut tip = tip.clone();
                tip.push(hint("Click: its GitHub page. Right-click: its folder."));
                tip.push(hint("Drag: another place. Middle-click: take it out."));
                tip
            }
            None => Lines::new(),
        },
        Spot::Strip(_) => Lines::new(),
        Spot::Undo => vec![("Undo".into(), slot.ink.0)],
        Spot::Edge => vec![hint("Drag to make it wider or narrower")],
        Spot::Tail => slot.pr_tip.clone(),
        Spot::Sheet => slot.summary.clone(),
        Spot::Off => Lines::new(),
    }
}

/// Whether the view is at rest on the screen: nothing of it grows,
/// changes windows or is hidden.
fn steady(slot: &OverlaySlot) -> bool {
    slot.next.is_none() && slot.anim.is_none() && slot.visible
}

/// The left button went down on `spot`.
fn down(spot: Spot, cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    match spot {
        Spot::Edge => start_resize(cx),
        Spot::Icon(row) => slot.icon_press = Some((row, win::cursor_pos())),
        Spot::Undo => slot.armed = Some(spot),
        // The sheet, and an empty spot of it: the view's or the badge's
        // to drag, and a click on the sheet opens the panel.
        Spot::Sheet | Spot::Tail => {
            slot.press_opens = true;
            press(cx);
        }
        Spot::Strip(_) => {
            slot.press_opens = false;
            press(cx);
        }
        Spot::Off => {}
    }
}

/// The mouse moved, the left button `pressed` or not: whatever it has
/// hold of follows. A move without the button means the release went
/// elsewhere (capture lost): what it had ends there.
fn moved(pressed: bool, cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    if !pressed {
        slot.armed = None;
        slot.icon_press = None;
    }
    if let Some((row, from)) = slot.icon_press {
        // A press on an icon that goes far enough takes its row.
        let at = win::cursor_pos();
        let far = (at.0 - from.0).abs().max((at.1 - from.1).abs()) >= layout::DRAG_THRESHOLD;
        if far {
            slot.icon_press = None;
            start_reorder(row, from.1, cx);
        }
        return;
    }
    if slot.resize.is_some() {
        if pressed {
            resize_to(cx);
        } else {
            end_resize(cx);
        }
    } else if slot.reorder.is_some() {
        if pressed {
            reorder_to(cx);
        } else {
            end_reorder(cx);
        }
    } else if slot.drag.is_some() {
        drag_to(pressed, cx);
    }
}

/// The left button came up on `spot`.
fn up(spot: Spot, click_count: Option<usize>, cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    if slot.resize.is_some() {
        end_resize(cx);
    } else if slot.reorder.is_some() {
        end_reorder(cx);
    } else if let Some((row, _)) = slot.icon_press.take() {
        // A click: the repo on GitHub.
        if spot == Spot::Icon(row) {
            open_row(row, true, cx);
        }
    } else if let Some(armed) = slot.armed.take() {
        // A click is a press and a release on the same button.
        if armed == spot && armed == Spot::Undo {
            undo(cx);
        }
    } else {
        release(click_count, cx);
    }
}

/// Opens a row's repo: on GitHub, or its folder. A repo only on GitHub
/// has no folder.
fn open_row(row: usize, web: bool, cx: &mut App) {
    let Some((path, _)) = cx.default_global::<OverlaySlot>().names.get(row).cloned() else {
        return;
    };
    let remote_only = cx.try_global::<Ui>().is_some_and(|ui| {
        let s = ui.state.read(cx);
        s.remote_statuses.iter().any(|r| r.path == path)
    });
    if web || remote_only {
        crate::repo_menu::open_github(&path, cx);
    } else {
        cx.open_with_system(&path);
    }
}

/// A middle click on a row's icon: asks, then takes the repo out of the
/// view.
fn ask_take_out(row: usize, cx: &mut App) {
    let Some((_, name)) = cx.default_global::<OverlaySlot>().names.get(row).cloned() else {
        return;
    };
    let asked = win::confirm_box(format!(
        "Take {name} out of the floating view?\n\nUndo brings it back, and so does pinning it \
         from its menu in the panel."
    ));
    cx.spawn(async move |cx| {
        let yes = cx
            .background_executor()
            .spawn(async move { asked.join().unwrap_or(false) })
            .await;
        if yes {
            cx.update(|cx| take_out(&name, cx)).ok();
        }
    })
    .detach();
}

/// Takes the repo listed as `name` out of the view.
fn take_out(name: &str, cx: &mut App) {
    let Some(mut lists) = lists_now(cx) else {
        return;
    };
    let before = lists.clone();
    let same = |n: &String| crate::config::same_name(n, name);
    lists.pinned.retain(|n| !same(n));
    lists.order.retain(|n| !same(n));
    if !lists.hidden.iter().any(same) {
        lists.hidden.push(name.to_string());
    }
    changed(before, lists, cx);
}

/// The self-test's way to take the first row out, as a middle click and
/// a yes would; whether there was one.
/// The window that shows the badge or the view now.
pub fn hwnd(cx: &App) -> Option<isize> {
    Some(cx.try_global::<OverlaySlot>()?.cur.as_ref()?.hwnd)
}

/// Where that window's inside starts on the screen, physical px.
pub fn origin(cx: &App) -> Option<(i32, i32)> {
    let area = cx.try_global::<OverlaySlot>()?.cur.as_ref()?.area?;
    Some((area.left, area.top))
}

/// The first row's tree on the screen, and its repo: for the self-test.
pub fn first_strip(cx: &App) -> Option<(Rect, PathBuf)> {
    let slot = cx.try_global::<OverlaySlot>()?;
    let r = slot.first_strip?;
    let area = slot.cur.as_ref()?.area?;
    let path = slot.names.first()?.0.clone();
    Some((
        Rect::new(
            area.left + r.left,
            area.top + r.top,
            area.left + r.right,
            area.top + r.bottom,
        ),
        path,
    ))
}

pub fn take_out_first(cx: &mut App) -> bool {
    let name = cx
        .default_global::<OverlaySlot>()
        .names
        .first()
        .map(|(_, name)| name.clone());
    if let Some(name) = &name {
        take_out(name, cx);
    }
    name.is_some()
}

/// Goes back one change, as the undo button does.
pub fn undo(cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    let Some(lists) = slot.undo.pop() else {
        return;
    };
    let now = Instant::now();
    if let Some(plate) = slot.plate.as_mut() {
        // Gone with the last change; kept while there are more.
        plate.until = if slot.undo.is_empty() {
            now.max(
                plate
                    .shown
                    .map_or(now, |t| t + Duration::from_secs_f32(PLATE_FADE)),
            )
        } else {
            now + PLATE_STAYS
        };
    }
    apply_lists(lists, cx);
    wake_plate(cx);
}

/// Whether the undo plate shows, or is on its way.
pub fn plate_showing(cx: &App) -> bool {
    cx.try_global::<OverlaySlot>()
        .is_some_and(|slot| slot.plate.is_some())
}

/// The view's lists changed here from `before` to `after`: kept as a
/// step to undo, and the undo plate shows for a while.
fn changed(before: FloatLists, after: FloatLists, cx: &mut App) {
    remember(before, cx);
    let slot = cx.default_global::<OverlaySlot>();
    let until = Instant::now() + PLATE_STAYS;
    match slot.plate.as_mut() {
        // Still going: it comes back from however far it went.
        Some(plate) => plate.until = until,
        None => slot.plate = Some(Plate { shown: None, until }),
    }
    apply_lists(after, cx);
    wake_plate(cx);
}

/// Wakes the view when the undo plate is to go, to fade it.
fn wake_plate(cx: &mut App) {
    let Some(plate) = cx.default_global::<OverlaySlot>().plate else {
        return;
    };
    let wait = plate.until.saturating_duration_since(Instant::now());
    cx.spawn(async move |cx| {
        cx.background_executor().timer(wait).await;
        cx.update(redraw).ok();
    })
    .detach();
}

/// The lists the view shows now: the ones changed here that the config
/// doesn't hold yet, else the config's.
fn lists_now(cx: &mut App) -> Option<FloatLists> {
    if let Some(lists) = cx.default_global::<OverlaySlot>().lists.clone() {
        return Some(lists);
    }
    let ui = cx.try_global::<Ui>()?;
    Some(FloatLists::of(&ui.state.read(cx).config))
}

/// Keeps `lists` as a step to go back to.
fn remember(lists: FloatLists, cx: &mut App) {
    let undo = &mut cx.default_global::<OverlaySlot>().undo;
    undo.push(lists);
    if undo.len() > UNDO_STEPS {
        undo.remove(0);
    }
}

/// The view lists `lists` from now on: at once here, and in the config
/// as soon as app.rs has saved them.
fn apply_lists(lists: FloatLists, cx: &mut App) {
    cx.default_global::<OverlaySlot>().lists = Some(lists.clone());
    if let Some(ui) = cx.try_global::<Ui>() {
        ui.input.unbounded_send(Input::FloatLists(lists)).ok();
    }
    // Windows are opened and placed outside the mouse event this came in.
    cx.defer(sync);
    redraw(cx);
}

/// A row is taken hold of by its icon, the press at `start` (screen y).
fn start_reorder(row: usize, start: i32, cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    if !steady(slot) {
        return;
    }
    let Some((path, _)) = slot.names.get(row).cloned() else {
        return;
    };
    slot.reorder = Some(Reorder {
        path,
        from: row,
        to: row,
        start,
        dy: 0,
    });
    sync_tooltip(Vec::new(), cx);
    reorder_to(cx);
    redraw(cx);
}

/// The row taken hold of follows the mouse, up and down among the rows.
fn reorder_to(cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    let (rows, strip) = (slot.names.len() as i32, slot.strip.max(1));
    let Some(r) = slot.reorder.as_mut() else {
        return;
    };
    let from = r.from as i32;
    let dy = (win::cursor_pos().1 - r.start).clamp(-from * strip, (rows - 1 - from) * strip);
    let to = ((from * strip + dy) as f32 / strip as f32).round() as i32;
    let to = to.clamp(0, (rows - 1).max(0)) as usize;
    if (dy, to) != (r.dy, r.to) {
        r.dy = dy;
        r.to = to;
        redraw(cx);
    }
}

/// The row is let go: it stays where it was brought, and every row
/// glides the rest of its way from where it is drawn now.
fn end_reorder(cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    let Some(r) = slot.reorder.take() else {
        return;
    };
    let strip = slot.strip.max(1) as f32;
    let old: Vec<(PathBuf, String)> = slot.names.clone();
    if r.from >= old.len() || r.to >= old.len() {
        redraw(cx);
        return;
    }
    // The dragged row glides on from where the mouse left it.
    let mut offsets: Vec<(PathBuf, f32)> = slot.offsets.clone();
    match offsets.iter_mut().find(|(p, _)| *p == r.path) {
        Some(o) => o.1 = r.dy as f32,
        None => offsets.push((r.path.clone(), r.dy as f32)),
    }
    if r.to == r.from {
        slot.offsets = offsets;
        redraw(cx);
        return;
    }
    let mut new = old.clone();
    let row = new.remove(r.from);
    new.insert(r.to, row);
    // Each row is drawn where it was, however its place changed.
    let drawn = |path: &Path| offsets.iter().find(|(p, _)| p == path).map_or(0.0, |o| o.1);
    slot.offsets = old
        .iter()
        .enumerate()
        .filter_map(|(i, (path, _))| {
            let j = new.iter().position(|(p, _)| p == path)?;
            let d = (i as f32 - j as f32) * strip + drawn(path);
            (d != 0.0).then(|| (path.clone(), d))
        })
        .collect();
    slot.offsets_at = None;
    slot.names = new.clone();
    let Some(mut lists) = lists_now(cx) else {
        return;
    };
    let before = lists.clone();
    lists.order = new.into_iter().map(|(_, name)| name).collect();
    changed(before, lists, cx);
}

/// The view's free side edge is taken hold of, to make it wider or
/// narrower. Its window grows to hold the widest it can get, and the
/// one the press landed in is kept while the drag lasts.
fn start_resize(cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    if !steady(slot) || slot.growth().0 < 1.0 {
        return;
    }
    let Some(width) = slot.cur.as_ref().map(|cur| cur.frame.width()) else {
        return;
    };
    slot.resize = Some((win::cursor_pos().0, width));
    slot.width = Some((width, true));
    hold(cx);
    sync_tooltip(Vec::new(), cx);
    cx.defer(sync);
}

/// The view is as wide as the mouse has dragged its edge.
fn resize_to(cx: &mut App) {
    let scale = view::sane_scale(win::primary_monitor().scale);
    let free_left = cx
        .default_global::<OverlaySlot>()
        .held
        .is_some_and(|(_, c)| matches!(c, Corner::TopRight | Corner::BottomRight));
    let slot = cx.default_global::<OverlaySlot>();
    let Some((start, from)) = slot.resize else {
        return;
    };
    let dx = win::cursor_pos().0 - start;
    let width = (if free_left { from - dx } else { from + dx }).clamp(
        place::px(crate::config::FLOAT_MIN_WIDTH, scale),
        place::px(crate::config::FLOAT_MAX_WIDTH, scale),
    );
    if slot.width != Some((width, true)) {
        slot.width = Some((width, true));
        cx.defer(sync);
    }
}

/// The edge is let go: the view keeps its width, and its window is cut
/// back to it.
fn end_resize(cx: &mut App) {
    let scale = view::sane_scale(win::primary_monitor().scale);
    let slot = cx.default_global::<OverlaySlot>();
    if slot.resize.take().is_none() {
        return;
    }
    let width = slot.width.map(|(width, _)| width);
    slot.width = width.map(|width| (width, false));
    let captor = slot.captor.take();
    if let (Some(width), Some(ui)) = (width, cx.try_global::<Ui>()) {
        ui.input
            .unbounded_send(Input::FloatWidth(width as f32 / scale))
            .ok();
    }
    if let Some(captor) = captor {
        // Not from inside its own mouse event.
        cx.defer(move |cx| view::remove(captor.handle.into(), cx));
    }
    cx.defer(drop_saved);
}

/// Left button down on the badge, or on the view's sheet: maybe a click,
/// maybe a drag.
fn press(cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    // Not while it changes windows or shape.
    if !steady(slot) {
        return;
    }
    if let Some(rect) = slot.shape() {
        slot.drag = Some(Drag::new(win::cursor_pos(), rect));
    }
}

/// The cursor moved while a press is on: once it is a drag, the badge
/// follows, kept on the primary work area. A move without the button means
/// the release went elsewhere (capture lost): end the press there.
fn drag_to(pressed: bool, cx: &mut App) {
    if !pressed {
        release(None, cx);
        return;
    }
    let work = win::primary_monitor().work;
    let slot = cx.default_global::<OverlaySlot>();
    let Some(drag) = slot.drag.as_mut() else {
        return;
    };
    if let Some(rect) = drag.track(win::cursor_pos(), work) {
        hold(cx);
        move_to(rect, cx);
    }
}

/// A drag has it: from here on it is kept where the drag puts it, by the
/// corner the config has, until the config holds the new place.
fn hold(cx: &mut App) {
    if cx.default_global::<OverlaySlot>().held.is_some() {
        return;
    }
    let corner = cx
        .try_global::<Ui>()
        .map(|ui| ui.state.read(cx).config.overlay.corner)
        .unwrap_or_default();
    let slot = cx.default_global::<OverlaySlot>();
    slot.held = slot.placed.map(|badge| (badge, corner));
}

/// Moves what shows (the badge, or the view) to `rect`, and its window
/// with it. Moves are coalesced: one task at a time, which takes the
/// latest target when it runs.
fn move_to(rect: Rect, cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    let Some(shape) = slot.shape() else { return };
    let (dx, dy) = (rect.left - shape.left, rect.top - shape.top);
    if (dx, dy) == (0, 0) {
        return;
    }
    let shift = |r: Rect| Rect::new(r.left + dx, r.top + dy, r.right + dx, r.bottom + dy);
    slot.placed = slot.placed.map(shift);
    slot.held = slot.held.map(|(badge, corner)| (shift(badge), corner));
    let Some(cur) = slot.cur.as_mut() else { return };
    cur.frame = shift(cur.frame);
    cur.plate = cur.plate.map(shift);
    cur.area = cur.area.map(shift);
    let (Some(area), h) = (cur.area, cur.hwnd) else {
        return;
    };
    if slot.move_to.replace(Some((area.left, area.top))).is_none() {
        let target = slot.move_to.clone();
        cx.spawn(async move |_| {
            if let Some((left, top)) = target.take() {
                unsafe { win::move_overlay(h, left, top) };
            }
        })
        .detach();
    }
}

/// The square `d` px wide in `sheet`'s corner nearest `corner` of the
/// screen: the badge the view there shrinks back to.
fn corner_of(sheet: Rect, corner: Corner, d: i32) -> Rect {
    let (left, top) = match corner {
        Corner::TopLeft => (sheet.left, sheet.top),
        Corner::TopRight => (sheet.right - d, sheet.top),
        Corner::BottomLeft => (sheet.left, sheet.bottom - d),
        Corner::BottomRight => (sheet.right - d, sheet.bottom - d),
    };
    Rect::new(left, top, left + d, top + d)
}

/// The press ended: a click opens the panel, a drag saves where the badge
/// was dropped. `click_count` is `None` when the release wasn't seen.
fn release(click_count: Option<usize>, cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    let Some(mut drag) = slot.drag.take() else {
        return;
    };
    let work = win::primary_monitor().work;
    if let Some(rect) = drag.track(win::cursor_pos(), work) {
        hold(cx);
        move_to(rect, cx);
    }
    let slot = cx.default_global::<OverlaySlot>();
    let input = if drag.is_drag() {
        // The view dropped: it is kept by the corner of the screen it
        // lies nearest now, and the badge is the view's own corner there,
        // so the view stays where it was dropped.
        if let Some(sheet) = slot.shape().filter(|_| slot.growth().0 >= 1.0)
            && let Some(badge) = slot.placed
        {
            let corner = layout::nearest_corner(sheet, work);
            let badge = corner_of(sheet, corner, badge.width());
            slot.placed = Some(badge);
            slot.held = Some((badge, corner));
        }
        slot.held.map(|(badge, _)| Input::OverlayMoved(badge))
    } else {
        // A double-click's second click would close the panel the first
        // one just opened. app.rs resolves overlay_click() and anchors the
        // panel.
        click_count
            .filter(|&n| n <= 1 && slot.press_opens)
            .map(|_| Input::Overlay)
    };
    if let (Some(input), Some(ui)) = (input, cx.try_global::<Ui>()) {
        ui.input.unbounded_send(input).ok();
    }
}

/// Where a dropped badge is kept: its corner and offsets in logical px.
/// app.rs saves them, then calls [`drop_saved`].
pub fn drop_position(rect: Rect) -> (Corner, f32, f32) {
    let mon = win::primary_monitor();
    layout::drop_position(rect, mon.work, view::sane_scale(mon.scale))
}

/// The config now holds the dropped position, so the badge goes back to
/// being placed from it (at the same spot).
pub fn drop_saved(cx: &mut App) {
    cx.default_global::<OverlaySlot>().held = None;
    sync(cx);
}

/// Whether the badge stays over the panel's window for now: while a flip
/// is drawn in it, unless the badge sits where the panel itself rests. The
/// badge shows over the big window, so it shows over the flip into it and
/// out of it too, and never blinks as the windows change hands.
fn badge_over_flip(cx: &App) -> bool {
    if !crate::flip::running(cx) {
        return false;
    }
    let badge = cx.try_global::<OverlaySlot>().and_then(|s| s.shape());
    match (badge, panel::last_rect(cx)) {
        (Some(b), Some(p)) => {
            !(b.left < p.right && p.left < b.right && b.top < p.bottom && p.top < b.bottom)
        }
        _ => true,
    }
}

/// The panel's window, when it belongs over the badge.
fn panel_over_badge(cx: &App) -> Option<isize> {
    panel::hwnd(cx).filter(|_| !badge_over_flip(cx))
}

/// Where the badge (or, grown, the view) is on the screen, physical px,
/// while it shows.
pub fn placed(cx: &App) -> Option<Rect> {
    let slot = cx.try_global::<OverlaySlot>()?;
    slot.shape().filter(|_| slot.visible)
}

/// A flip starts in the panel's window: the badge goes over it, where it
/// stays over the big window.
pub fn over_flip(cx: &mut App) {
    if !badge_over_flip(cx) {
        return;
    }
    let slot = cx.default_global::<OverlaySlot>();
    let mut panes: Vec<isize> = [&slot.cur, &slot.next]
        .into_iter()
        .flatten()
        .map(|pane| pane.hwnd)
        .collect();
    let visible = slot.visible;
    panes.extend(crate::float_tip::shown_hwnd(cx));
    if visible {
        cx.spawn(async move |_| {
            for h in panes {
                unsafe { win::reassert_topmost(h) };
            }
        })
        .detach();
    }
}

/// What the view's size comes from.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Held {
    /// Its width, device px.
    width: i32,
    radius: f32,
    rows: usize,
    health: bool,
    /// The lines after its rows.
    tail: usize,
}

/// The view's sheet on the screen, grown from the badge at `badge` kept
/// by `corner`: as wide as asked and as tall as its rows, as far as the
/// work area has room that way.
fn grown_frame(badge: Rect, corner: Corner, work: Rect, scale: f32, view: Held) -> Rect {
    let (left, top) = match corner {
        Corner::TopLeft => (true, true),
        Corner::TopRight => (false, true),
        Corner::BottomLeft => (true, false),
        Corner::BottomRight => (false, false),
    };
    let room_w = if left {
        work.right - badge.left
    } else {
        badge.right - work.left
    };
    let room_h = if top {
        work.bottom - badge.top
    } else {
        badge.bottom - work.top
    };
    let l = place::layout(&place::Spec {
        scale,
        badge: badge.width(),
        width: view.width.min(room_w),
        max_height: room_h,
        radius: view.radius,
        rows: view.rows,
        health: view.health,
        buttons: 1,
        tail: view.tail,
        scroll: 0,
    });
    place::frame(badge, l.size, corner)
}

/// Idempotent: opens, closes, places, shows or hides the badge to match
/// the config and the state, grown or not. A window that is no longer
/// where or as big as it has to be is replaced by one that is.
pub fn sync(cx: &mut App) {
    let Some(state) = cx.try_global::<Ui>().map(|ui| ui.state.clone()) else {
        return;
    };
    let s = state.read(cx);
    let cfg = &s.config;
    if !cfg.overlay.enabled {
        close(cx);
        return;
    }
    let (corner, diameter, offset) = (cfg.overlay.corner, cfg.overlay_size(), cfg.overlay_offset());
    let hide = (cfg.overlay.hide_when_synced && s.badge() == Badge::AllSynced)
        || (cfg.overlay.hide_in_fullscreen && win::fullscreen_busy());
    let want_grown = cfg.overlay.grown;
    let mon = win::primary_monitor();
    let scale = view::sane_scale(mon.scale);
    // What was changed here stands until the config holds it.
    let cfg_lists = FloatLists::of(cfg);
    let cfg_width = place::px(cfg.float_width(), scale);
    let (held_lists, held_width) = match cx.try_global::<OverlaySlot>() {
        Some(slot) => (
            slot.lists.clone().filter(|l| *l != cfg_lists),
            slot.width.filter(|w| w.1 || w.0 != cfg_width),
        ),
        None => (None, None),
    };
    let view = Held {
        width: held_width.map_or(cfg_width, |w| w.0),
        radius: cfg.overlay_radius(),
        rows: s
            .float_repos_of(held_lists.as_ref().unwrap_or(&cfg_lists), unix_now())
            .len(),
        health: cfg.float.health,
        tail: usize::from(show_prs(cfg) && !s.open_prs().is_empty()),
    };
    // While its edge is dragged its window holds the widest it can get.
    let widest = Held {
        width: place::px(crate::config::FLOAT_MAX_WIDTH, scale),
        ..view
    };
    let ink = cx
        .try_global::<OverlaySlot>()
        .map_or_else(|| (gpui::black(), gpui::black()), |slot| slot.ink);
    let summary = tooltip_text(s, ink);

    ensure_open(state.clone(), cx);
    ensure_timer(cx);

    let physical = |v: f32| (v * scale).round() as i32;
    let pad = layout::badge_pad(scale);
    let panel = panel_over_badge(cx);
    let slot = cx.default_global::<OverlaySlot>();
    slot.summary = summary;
    slot.lists = held_lists;
    slot.width = held_width;
    if slot.drag.is_some_and(|d| d.is_drag()) {
        // The drag moves it.
        return;
    }
    let (badge, corner) = slot.held.unwrap_or_else(|| {
        let badge = layout::corner(
            mon.work,
            corner,
            physical(diameter),
            (physical(offset.0), physical(offset.1)),
        );
        (badge, corner)
    });
    let frame = grown_frame(badge, corner, mon.work, scale, view);

    // Asked to grow or to shrink: from however far grown it is now.
    match slot.grown {
        None => slot.grown = Some(want_grown),
        Some(grown) if grown != want_grown => {
            let (from, _) = slot.growth();
            slot.grown = Some(want_grown);
            slot.anim = Some((from, None));
        }
        Some(_) => {}
    }
    let (g, _) = slot.growth();
    // The window holds the view from before it starts to grow until
    // after it has shrunk.
    let big = want_grown || g > 0.0 || slot.anim.is_some();
    let sizing = big && slot.width.is_some_and(|w| w.1);
    let held = if sizing {
        grown_frame(badge, corner, mon.work, scale, widest)
    } else if big {
        frame
    } else {
        badge
    };
    // The undo plate, beside the view at rest, until it has faded.
    if slot.plate_gone() {
        slot.plate = None;
    }
    let grown_still = big && g >= 1.0 && slot.anim.is_none() && !sizing;
    if !grown_still && slot.anim.is_some() {
        slot.plate = None;
    }
    let plate = slot
        .plate
        .filter(|_| grown_still)
        .map(|_| place::plate(frame, mon.work, scale));
    let around = match plate {
        Some(p) => Rect::new(
            held.left.min(p.left),
            held.top.min(p.top),
            held.right.max(p.right),
            held.bottom.max(p.bottom),
        ),
        None => held,
    };
    let area = around.outset(pad);
    // At rest it is cut to the sheet's own shape (and the plate's); while
    // that changes, to nothing.
    let local = |r: Rect| {
        let r = r.outset(pad);
        Rect::new(
            r.left - area.left,
            r.top - area.top,
            r.right - area.left,
            r.bottom - area.top,
        )
    };
    let shape = (slot.anim.is_none() && !sizing).then(|| {
        let mut cut = vec![(
            local(held),
            physical(view.radius).min(badge.width() / 2) + pad,
        )];
        if let Some(p) = plate {
            cut.push((local(p), physical(place::PLATE_RADIUS) + pad));
        }
        cut
    });
    slot.placed = Some(badge);
    slot.pad = pad;
    let Some(cur) = slot.cur.as_mut() else { return };
    let h = cur.hwnd;
    if hide {
        if slot.visible {
            slot.visible = false;
            slot.drag = None;
            cx.spawn(async move |_| unsafe { win::hide(h) }).detach();
        }
        return;
    }
    let mut refresh = false;
    if !slot.visible || cur.area.is_none() {
        // Not on the screen: placed as it is to be, and shown, topmost
        // and without activation. An open panel goes back above it in
        // the same task, so the badge never covers it.
        slot.visible = true;
        cur.area = Some(area);
        cur.frame = frame;
        cur.plate = plate;
        cur.shape = Some(shape.clone());
        refresh = true;
        cx.spawn(async move |_| unsafe {
            win::place_overlay(h, area, shape.as_deref());
            if let Some(p) = panel {
                win::panel_restack(p);
            }
        })
        .detach();
    } else if cur.area != Some(area) {
        // On the screen, and to be elsewhere or another size: a new
        // window comes up there, cloaked, and takes over once it has
        // drawn the same picture.
        if let Some(next) = slot.next.as_mut().filter(|n| n.area == Some(area))
            && (next.frame, next.plate) != (frame, plate)
        {
            next.frame = frame;
            next.plate = plate;
            refresh = true;
        }
        if slot.next.is_none()
            && let Some((handle, hwnd)) = open(state, true, cx)
        {
            cx.default_global::<OverlaySlot>().next = Some(Pane {
                handle,
                hwnd,
                area: Some(area),
                frame,
                plate,
                shape: Some(shape.clone()),
                drawn: 0,
            });
            cx.spawn(async move |_| unsafe {
                win::style_overlay(hwnd);
                win::place_overlay(hwnd, area, shape.as_deref());
                if let Some(p) = panel {
                    win::panel_restack(p);
                }
            })
            .detach();
        }
    } else {
        let reshape = cur.shape.as_ref() != Some(&shape);
        cur.shape = Some(shape.clone());
        // The same window, the view in it wider or narrower.
        if (cur.frame, cur.plate) != (frame, plate) {
            cur.frame = frame;
            cur.plate = plate;
            refresh = true;
        }
        // The window holds the plate: it comes now.
        if let Some(p) = slot.plate.as_mut().filter(|p| p.shown.is_none())
            && plate.is_some()
        {
            p.shown = Some(Instant::now());
            refresh = true;
        }
        // The window holds it: a growth waiting for that sets off.
        if let Some((from, None)) = slot.anim {
            slot.anim = Some((from, Some(Instant::now())));
            refresh = true;
        }
        if reshape {
            cx.spawn(async move |_| unsafe { win::shape_overlay(h, shape.as_deref()) })
                .detach();
        }
    }
    if refresh {
        redraw(cx);
    }
    let text = tip_for(cx.default_global::<OverlaySlot>());
    sync_tooltip(text, cx);
    crate::float_tip::sync(cx);
    // The view's trees hear the wheel though it never has the focus.
    let slot = cx.default_global::<OverlaySlot>();
    let view = slot.cur.as_ref().map(|cur| cur.hwnd);
    win::wheel_to(view.filter(|_| slot.grown == Some(true)));
}

/// What the view says on hover, off its rows: the tray's summary, then a
/// line per repo that isn't synced.
fn tooltip_text(s: &AppState, (text, muted): (Hsla, Hsla)) -> Lines {
    let problem = s.problem();
    let summary = crate::tray::summary(
        &crate::tray::TraySnapshot {
            statuses: &s.statuses,
            checking: s.sweeping,
            has_result: s.has_result,
            problem: problem.as_deref(),
            incomplete: s.capped.is_some(),
        },
        s.unsynced(),
    );
    let mut lines = vec![(format!("gitstat: {summary}"), text)];
    if let Some(note) = s.badge_note() {
        lines.push((format!("Badge: {note}"), muted));
    }
    const SHOWN: usize = 12;
    let unsynced: Vec<_> = s
        .sorted_for_panel()
        .into_iter()
        .filter(|r| !r.is_synced())
        .collect();
    for r in unsynced.iter().take(SHOWN) {
        lines.push((format!("{}: {}", r.name, r.label()), muted));
    }
    if unsynced.len() > SHOWN {
        lines.push((format!("…and {} more", unsynced.len() - SHOWN), muted));
    }
    lines
}

/// Asks for the tooltip `lines` say, none when empty. As GPUI's own: it
/// shows once the mouse has rested on what it is of for a moment, a px
/// off the mouse; one that shows takes new words in place.
fn sync_tooltip(lines: Lines, cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    if slot.tip_text == lines {
        return;
    }
    // The same thing says something new: in place. Another: after the
    // wait, where the mouse is then.
    let same_spot = slot.tip_spot == slot.spot && !slot.tip_text.is_empty() && !lines.is_empty();
    slot.tip_spot = slot.spot;
    slot.tip_text = lines.clone();
    slot.tip_asked += 1;
    if let (true, Some(at)) = (same_spot, slot.tip_at) {
        // The same thing says something new.
        crate::float_tip::set_lines(Some((lines, at)), cx);
        return;
    }
    slot.tip_at = None;
    let asked = slot.tip_asked;
    crate::float_tip::set_lines(None, cx);
    if lines.is_empty() {
        return;
    }
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(crate::float_tip::DELAY)
            .await;
        cx.update(|cx| {
            let slot = cx.default_global::<OverlaySlot>();
            if slot.tip_asked != asked || !steady(slot) {
                return;
            }
            let at = win::cursor_pos();
            slot.tip_at = Some(at);
            let lines = slot.tip_text.clone();
            crate::float_tip::set_lines(Some((lines, at)), cx);
        })
        .ok();
    })
    .detach();
}

/// Removes the badge and stops its timer.
pub fn close(cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    let panes = [slot.cur.take(), slot.next.take(), slot.captor.take()];
    slot.resize = None;
    slot.reorder = None;
    slot.plate = None;
    slot.placed = None;
    slot.visible = false;
    slot.drag = None;
    slot.held = None;
    slot.timer = None;
    slot.tip_text.clear();
    slot.tip_at = None;
    slot.grown = None;
    slot.anim = None;
    for pane in panes.into_iter().flatten() {
        view::remove(pane.handle.into(), cx);
    }
    crate::float_tip::close(cx);
    win::wheel_to(None);
}

/// The panel's anchor for a badge click: the badge's rect and corner, or
/// `None` while it isn't showing (then anchor at the tray instead).
pub fn anchor(cx: &App) -> Option<Anchor> {
    let slot = cx.try_global::<OverlaySlot>()?;
    let rect = slot.shape().filter(|_| slot.visible)?;
    let corner: Corner = match slot.held {
        Some((_, corner)) => corner,
        None => cx.try_global::<Ui>()?.state.read(cx).config.overlay.corner,
    };
    Some(Anchor::Overlay { rect, corner })
}

/// Opens a window for the badge: hidden, to be placed and shown from a
/// task; `cloaked`, unseen even then, until it takes another's place.
fn open(
    state: Entity<AppState>,
    cloaked: bool,
    cx: &mut App,
) -> Option<(WindowHandle<OverlayView>, isize)> {
    let mut hwnd = None;
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(64.), px(64.)),
                ))),
                titlebar: None,
                focus: false,
                show: false,
                kind: WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Transparent,
                ..Default::default()
            },
            |window, cx| {
                // Only read the HWND here; styling sends messages, so it
                // runs in a task once this update has returned.
                hwnd = win::hwnd(window);
                if let Some(h) = hwnd.filter(|_| cloaked) {
                    win::cloak(h);
                }
                cx.new(|cx| OverlayView::new(state, cx))
            },
        )
        .ok()?;
    Some((handle, hwnd?))
}

fn ensure_open(state: Entity<AppState>, cx: &mut App) {
    if cx
        .try_global::<OverlaySlot>()
        .and_then(|slot| slot.cur.as_ref().map(|cur| cur.handle))
        .is_some_and(|h| view::is_live(h.into(), cx))
    {
        return;
    }
    if std::env::var_os("GPUI_DISABLE_DIRECT_COMPOSITION").is_some() {
        eprintln!(
            "gitstat: GPUI_DISABLE_DIRECT_COMPOSITION is set; the badge will draw on a black square"
        );
    }
    let opened = open(state, false, cx);
    if let Some((_, h)) = opened {
        cx.spawn(async move |_| unsafe { win::style_overlay(h) })
            .detach();
    }
    let slot = cx.default_global::<OverlaySlot>();
    let next = slot.next.take();
    *slot = OverlaySlot {
        cur: opened.map(|(handle, hwnd)| Pane {
            handle,
            hwnd,
            area: None,
            frame: Rect::new(0, 0, 0, 0),
            plate: None,
            shape: None,
            drawn: 0,
        }),
        timer: slot.timer,
        timers_started: slot.timers_started,
        ..Default::default()
    };
    if let Some(next) = next {
        view::remove(next.handle.into(), cx);
    }
}

fn ensure_timer(cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    if slot.timer.is_some() {
        return;
    }
    slot.timers_started += 1;
    let id = slot.timers_started;
    slot.timer = Some(id);
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(TICK).await;
            if !cx.update(|cx| tick(id, cx)).unwrap_or(false) {
                break;
            }
        }
    })
    .detach();
}

/// One timer pass: Win32 queries only, no git. Returns false once this
/// timer should stop.
fn tick(id: u64, cx: &mut App) -> bool {
    let Some(slot) = cx.try_global::<OverlaySlot>() else {
        return false;
    };
    if slot.timer != Some(id) {
        return false;
    }
    // A press whose release never arrived (capture taken away mid-drag)
    // ends here rather than leaving the badge stuck to a drag.
    if slot.drag.is_some() && !win::left_button_down() {
        let slot = cx.default_global::<OverlaySlot>();
        if slot.drag.as_mut().is_some_and(|d| d.stale()) {
            release(None, cx);
        }
    }
    // So does the hold of a row or of the view's edge.
    let slot = cx.default_global::<OverlaySlot>();
    if (slot.resize.is_some() || slot.reorder.is_some()) && !win::left_button_down() {
        moved(false, cx);
    }
    let slot = cx.default_global::<OverlaySlot>();
    if slot.resize.is_none()
        && let Some(captor) = slot.captor.take()
    {
        view::remove(captor.handle.into(), cx);
    }
    let slot = cx.default_global::<OverlaySlot>();
    // Something other than us may have hidden or shown it; sync corrects
    // whatever it finds.
    if let Some(h) = slot.cur.as_ref().map(|cur| cur.hwnd) {
        let shown = win::is_visible(h);
        cx.default_global::<OverlaySlot>().visible = shown;
    }
    // A window that never came to take over (it could not be placed, or
    // never drew) is let go after two passes, and tried again.
    let slot = cx.default_global::<OverlaySlot>();
    let stuck = slot.next.take_if(|next| {
        next.drawn += 100;
        next.drawn >= 200
    });
    if let Some(next) = stuck {
        view::remove(next.handle.into(), cx);
    }
    sync(cx);
    let slot = cx.default_global::<OverlaySlot>();
    if slot.timer != Some(id) {
        return false;
    }
    let shown = slot.cur.as_ref().map(|cur| cur.hwnd);
    if let Some(h) = shown.filter(|_| slot.visible) {
        let panel = panel_over_badge(cx);
        let tip = crate::float_tip::shown_hwnd(cx);
        cx.spawn(async move |_| unsafe {
            win::reassert_topmost(h);
            // and what it says over it.
            if let Some(tip) = tip {
                win::reassert_topmost(tip);
            }
            // Topmost puts the badge above the panel; put the panel back.
            if let Some(p) = panel {
                win::panel_restack(p);
            }
        })
        .detach();
    }
    view::hide_keepalive_if_shown(cx);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_fades_while_the_sheet_moves() {
        for i in 0..=1000 {
            let p = pose(i as f32 / 1000.0);
            assert!(p.face == 0.0 || p.sheet == 0.0);
            assert!(p.rows == 0.0 || p.sheet == 1.0);
        }
        assert_eq!(
            pose(0.0),
            Pose {
                face: 1.0,
                sheet: 0.0,
                rows: 0.0
            }
        );
        assert_eq!(
            pose(1.0),
            Pose {
                face: 0.0,
                sheet: 1.0,
                rows: 1.0
            }
        );
    }
}
