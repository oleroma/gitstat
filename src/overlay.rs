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
use crate::state::{AppState, Input};
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

/// A repo's row in the view.
struct RowPart {
    repo: Arc<RepoStatus>,
    color: Hsla,
    /// Its tree, and where the mouse finds its icon.
    graph: Option<AnyElement>,
    label: Option<AnyElement>,
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
}

struct Laid {
    sheet: Rect,
    picture: Picture,
    slots: Vec<AnyElement>,
    list: Option<Rect>,
    /// Each row, cut to the list: where the mouse is over it.
    rows: Vec<Rect>,
    max_scroll: i32,
    strip: i32,
}

fn contains(r: Rect, (x, y): (i32, i32)) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
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
        let mut rows = Vec::new();
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
                tail: 0,
                scroll: self.scroll,
            });
            // Scrolled past its end (rows went): back to it.
            if l.scroll != self.scroll {
                cx.default_global::<OverlaySlot>().scroll = l.scroll;
            }
            max_scroll = l.max_scroll;
            let sh =
                |r: Rect| Rect::new(r.left + at.0, r.top + at.1, r.right + at.0, r.bottom + at.1);
            let faded = |mut part: Part| {
                part.alpha = alpha;
                part
            };
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
                        "Pin repos from their menu",
                        Look::new(TINY, p.muted),
                    )
                    .centred(),
                ));
            }
            let in_sight = |r: Rect| r.bottom > l.list.top && r.top < l.list.bottom;
            for (part, row) in self.rows.iter().zip(&l.rows) {
                if !in_sight(row.rect) {
                    continue;
                }
                parts.push(faded(
                    Part::new(
                        Key::Icon(part.repo.path.clone()),
                        sh(row.icon),
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
            let list_bounds = paint::bounds(l.list, at, s);
            window.with_content_mask(
                Some(ContentMask {
                    bounds: list_bounds,
                }),
                |window| {
                    for (part, row) in self.rows.iter_mut().zip(&l.rows) {
                        if !in_sight(row.rect) {
                            continue;
                        }
                        if let Some(slot) = part.graph.take() {
                            slots.push(view::put(slot, row.strip, at, s, alpha, window, cx));
                        }
                        if let Some(slot) = part.label.take() {
                            slots.push(view::put(slot, row.label, at, s, alpha, window, cx));
                        }
                    }
                },
            );
            let shown = sh(l.list);
            rows = l
                .rows
                .iter()
                .map(|row| {
                    let r = sh(row.rect);
                    let top = r.top.max(shown.top);
                    Rect::new(r.left, top, r.right, r.bottom.min(shown.bottom).max(top))
                })
                .collect();
            list = Some(shown);
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
            rows,
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
        let rows = Rc::new(std::mem::take(&mut laid.rows));
        let list = laid.list.filter(|_| self.pose.rows >= 1.0);
        let (max_scroll, strip) = (laid.max_scroll, laid.strip);

        // Before the rows paint: their own handlers (a tree's dots, an
        // icon's menu) then come first, and what they don't take lands
        // here. Window-wide, hit or not: with the mouse captured, moves
        // and the release outside the window land here too.
        let over_rows = rows.clone();
        window.on_mouse_event(move |e: &MouseDownEvent, phase, _, cx| {
            let at = dev(e.position);
            if phase != DispatchPhase::Bubble || !contains(sheet, at) {
                return;
            }
            let on_row =
                list.is_some_and(|l| contains(l, at)) && over_rows.iter().any(|r| contains(*r, at));
            match e.button {
                // The rows are for their trees and icons; everything
                // else of the sheet takes hold of it.
                MouseButton::Left if !on_row => press(cx),
                MouseButton::Right => toggle(cx),
                _ => {}
            }
        });
        let hover_rows = rows.clone();
        window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let pressed = e.pressed_button == Some(MouseButton::Left);
            if cx.default_global::<OverlaySlot>().drag.is_some() {
                drag_to(pressed, cx);
            }
            let at = dev(e.position);
            let row = list
                .filter(|l| contains(*l, at))
                .and_then(|_| hover_rows.iter().position(|r| contains(*r, at)));
            hover(row, contains(sheet, at), cx);
        });
        window.on_mouse_event(|e: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble && e.button == MouseButton::Left {
                release(Some(e.click_count), cx);
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
        let slot = cx.default_global::<OverlaySlot>();
        let (g, moving) = slot.growth();
        let pose = pose(g);
        let scroll = slot.scroll;
        // Where this window is, and so where in it the badge and the view
        // are: nowhere before it is placed.
        let placed = slot
            .pane(hwnd)
            .and_then(|pane| Some((pane.area?, pane.frame)))
            .zip(slot.placed);
        let Some(((area, frame), badge)) = placed else {
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
        let health = cfg
            .float
            .health
            .then(|| (view::tone_counts(&state.statuses), state.sweeping));
        let mut rows = Vec::new();
        let mut tips = Vec::new();
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
            for (i, s) in state.float_repos(unix_now()).into_iter().enumerate() {
                let color = view::status_color(s, &p);
                let (graph, label, checkout, status) = match &self.timelines {
                    Some(t) => {
                        let strip = Timelines::strip(t, i, s, &strip_p, dark, true, true, true, cx);
                        (
                            Some(strip.graph),
                            Some(strip.label),
                            strip.checkout,
                            strip.status,
                        )
                    }
                    None => (None, None, String::new(), s.label()),
                };
                let mut tip = s.name.clone();
                for line in [checkout, status]
                    .into_iter()
                    .chain(view::status_details(s, &p).into_iter().map(|(l, _)| l))
                    .filter(|l| !l.is_empty())
                {
                    tip.push('\n');
                    tip.push_str(&line);
                }
                tips.push(tip);
                rows.push(RowPart {
                    repo: Arc::new(s.clone()),
                    color,
                    graph,
                    label,
                });
            }
        }
        cx.default_global::<OverlaySlot>().tips = tips;
        Scene {
            hwnd,
            p,
            fill,
            text: text.into(),
            pose,
            moving,
            badge: local(badge),
            frame: local(frame),
            radius,
            opacity,
            health,
            rows,
            scroll,
        }
        .into_any_element()
    }
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
    /// The view's sheet on the screen, as this window holds it.
    frame: Rect,
    /// How it is cut, as last asked: see `win::shape_overlay`.
    shape: Option<Option<i32>>,
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
    /// The native tooltip of `cur`: its HWND once made, and the text it
    /// was last given. The window is no bigger than what it shows, so a
    /// tooltip GPUI drew would have no room.
    tip: Option<isize>,
    tip_pending: bool,
    tip_text: String,
    /// Whether it is the view (or on its way to it); none before the
    /// first sync.
    grown: Option<bool>,
    /// On its way there: how far grown it set off, and when. No time yet
    /// while the window that holds the view is still coming up.
    anim: Option<(f32, Option<Instant>)>,
    /// How far the view's rows are scrolled, physical px.
    scroll: i32,
    /// The row under the mouse, and whether the mouse is on the sheet.
    hover: (Option<usize>, bool),
    /// What each row says under the mouse, and the view as a whole.
    tips: Vec<String>,
    summary: String,
}

impl Global for OverlaySlot {}

impl OverlaySlot {
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
        [&self.cur, &self.next]
            .into_iter()
            .flatten()
            .find(|pane| pane.hwnd == hwnd)
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
    slot.tip = None;
    slot.tip_text.clear();
    win::dwm_flush();
    win::uncloak(new);
    if let Some(old) = old {
        win::cloak(old.hwnd);
        view::remove(old.handle.into(), cx);
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
    for handle in handles {
        handle.update(cx, |_, window, _| window.refresh()).ok();
    }
}

/// A right click on the badge or the view: the one becomes the other.
fn toggle(cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    if slot.drag.is_some() {
        return;
    }
    let grown = slot.grown == Some(true);
    if let Some(ui) = cx.try_global::<Ui>() {
        ui.input.unbounded_send(Input::OverlayGrown(!grown)).ok();
    }
}

/// The mouse moved: what it rests on says what it is.
fn hover(row: Option<usize>, on_sheet: bool, cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    if slot.hover == (row, on_sheet) {
        return;
    }
    slot.hover = (row, on_sheet);
    let text = tip_for(slot);
    sync_tooltip(text, cx);
}

/// What the tooltip says now: the row under the mouse, else, grown, the
/// tray's summary.
fn tip_for(slot: &OverlaySlot) -> String {
    if slot.growth().0 < 1.0 {
        return String::new();
    }
    match slot.hover {
        (Some(row), _) => slot.tips.get(row).cloned().unwrap_or_default(),
        _ => slot.summary.clone(),
    }
}

/// Left button down on the badge, or on the view's sheet: maybe a click,
/// maybe a drag.
fn press(cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    // Not while it changes windows or shape.
    if slot.next.is_some() || slot.anim.is_some() || !slot.visible {
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
        click_count.filter(|&n| n <= 1).map(|_| Input::Overlay)
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

/// Opens the big window at a repo of the view's, through the panel when
/// the flip is on; the repo itself when the big window is turned off.
pub fn open_repo(path: &std::path::Path, cx: &mut App) {
    let Some(ui) = cx.try_global::<Ui>() else {
        return;
    };
    if !ui.state.read(cx).config.window {
        view::open_repo(path, cx);
        return;
    }
    let timelines = ui.timelines.clone();
    timelines.update(cx, |t, _| t.focus(path));
    let anchor = anchor(cx).unwrap_or(Anchor::Tray(None));
    crate::flip::show_window(anchor, cx);
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
    let panes: Vec<isize> = [&slot.cur, &slot.next]
        .into_iter()
        .flatten()
        .map(|pane| pane.hwnd)
        .collect();
    if slot.visible {
        cx.spawn(async move |_| {
            for h in panes {
                unsafe { win::reassert_topmost(h) };
            }
        })
        .detach();
    }
}

/// The view's sheet on the screen, grown from the badge at `badge` kept
/// by `corner`: as wide as asked and as tall as its rows, as far as the
/// work area has room that way.
fn grown_frame(
    badge: Rect,
    corner: Corner,
    work: Rect,
    scale: f32,
    (width, radius, rows, health): (f32, f32, usize, bool),
) -> Rect {
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
        width: place::px(width, scale).min(room_w),
        max_height: room_h,
        radius,
        rows,
        health,
        buttons: 0,
        tail: 0,
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
    let view = (
        cfg.float_width(),
        cfg.overlay_radius(),
        s.float_repos(unix_now()).len(),
        cfg.float.health,
    );
    let summary = tooltip_text(s);

    ensure_open(state.clone(), cx);
    ensure_timer(cx);

    let mon = win::primary_monitor();
    let scale = view::sane_scale(mon.scale);
    let physical = |v: f32| (v * scale).round() as i32;
    let pad = layout::badge_pad(scale);
    let panel = panel_over_badge(cx);
    let slot = cx.default_global::<OverlaySlot>();
    slot.summary = summary;
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
    let area = if big { frame } else { badge }.outset(pad);
    // At rest it is cut to the sheet's own shape; while that changes, to
    // nothing.
    let shape = slot
        .anim
        .is_none()
        .then(|| physical(view.1).min(badge.width() / 2) + pad);
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
        cur.shape = Some(shape);
        refresh = true;
        cx.spawn(async move |_| unsafe {
            win::place_overlay(h, area, shape);
            if let Some(p) = panel {
                win::panel_restack(p);
            }
        })
        .detach();
    } else if cur.area != Some(area) {
        // On the screen, and to be elsewhere or another size: a new
        // window comes up there, cloaked, and takes over once it has
        // drawn the same picture.
        if slot.next.is_none()
            && let Some((handle, hwnd)) = open(state, true, cx)
        {
            cx.default_global::<OverlaySlot>().next = Some(Pane {
                handle,
                hwnd,
                area: Some(area),
                frame,
                shape: Some(shape),
                drawn: 0,
            });
            cx.spawn(async move |_| unsafe {
                win::style_overlay(hwnd);
                win::place_overlay(hwnd, area, shape);
                if let Some(p) = panel {
                    win::panel_restack(p);
                }
            })
            .detach();
        }
    } else {
        let reshape = cur.shape != Some(shape);
        cur.shape = Some(shape);
        // The window holds it: a growth waiting for that sets off.
        if let Some((from, None)) = slot.anim {
            slot.anim = Some((from, Some(Instant::now())));
            refresh = true;
        }
        if reshape {
            cx.spawn(async move |_| unsafe { win::shape_overlay(h, shape) })
                .detach();
        }
    }
    if refresh {
        redraw(cx);
    }
    let text = tip_for(cx.default_global::<OverlaySlot>());
    sync_tooltip(text, cx);
}

/// What the view says on hover, off its rows: the tray's summary, then a
/// line per repo that isn't synced.
fn tooltip_text(s: &AppState) -> String {
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
    let mut text = format!("gitstat: {summary}");
    if let Some(note) = s.badge_note() {
        text.push_str(&format!("\nBadge: {note}"));
    }
    const SHOWN: usize = 12;
    let unsynced: Vec<_> = s
        .sorted_for_panel()
        .into_iter()
        .filter(|r| !r.is_synced())
        .collect();
    for r in unsynced.iter().take(SHOWN) {
        text.push_str(&format!("\n{}: {}", r.name, r.label()));
    }
    if unsynced.len() > SHOWN {
        text.push_str(&format!("\n…and {} more", unsynced.len() - SHOWN));
    }
    text
}

/// Gives the badge its tooltip text, making the tooltip first if needed.
/// An empty text shows none.
fn sync_tooltip(text: String, cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    let Some(h) = slot.cur.as_ref().map(|cur| cur.hwnd) else {
        return;
    };
    if slot.tip_text == text && (slot.tip.is_some() || text.is_empty()) {
        return;
    }
    match slot.tip {
        Some(tip) => {
            slot.tip_text = text.clone();
            cx.spawn(async move |_| unsafe { win::set_tooltip(tip, h, &text) })
                .detach();
        }
        None if text.is_empty() || slot.tip_pending => {}
        None => {
            slot.tip_pending = true;
            cx.spawn(async move |cx| {
                let tip = unsafe { win::create_tooltip(h) };
                cx.update(|cx| {
                    let slot = cx.default_global::<OverlaySlot>();
                    slot.tip_pending = false;
                    // The badge was closed or made again meanwhile; the
                    // tooltip went with its owner.
                    if slot.cur.as_ref().map(|cur| cur.hwnd) != Some(h) {
                        return;
                    }
                    slot.tip = tip;
                    slot.tip_text = String::new();
                    sync(cx);
                })
                .ok();
            })
            .detach();
        }
    }
}

/// Removes the badge and stops its timer.
pub fn close(cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    let panes = [slot.cur.take(), slot.next.take()];
    slot.placed = None;
    slot.visible = false;
    slot.drag = None;
    slot.held = None;
    slot.timer = None;
    slot.tip = None;
    slot.tip_text.clear();
    slot.grown = None;
    slot.anim = None;
    for pane in panes.into_iter().flatten() {
        view::remove(pane.handle.into(), cx);
    }
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
        cx.spawn(async move |_| unsafe {
            win::reassert_topmost(h);
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
