//! The flip animation: opening the big window from the panel turns the
//! panel into it, and closing the big window turns it back.
//!
//! The panel and the big window are two OS windows, and resizing a window
//! every frame flickers, so neither moves. A third window, borderless,
//! transparent, click-through and topmost, covers both their places for
//! the length of the flip and draws it. Opening:
//!
//! 1. Fade: a sheet the colour of the strips fades in over the panel,
//!    hiding its words, while each repo's tree is drawn over its strip.
//! 2. Turn: the panel closes; the sheet grows from the panel's place to
//!    the big window's, each tree turns a quarter turn from its strip to
//!    where its column will be (`timeline::paint_tree`), and each section's
//!    heading stretches into its spine.
//! 3. Hold: the big window opens underneath, still covered, and gets a
//!    moment to draw.
//! 4. Reveal: the sheet and the trees fade out, showing the big window
//!    with its words, then the flip's window closes.
//!
//! Closing or minimising runs the same backwards: the sheet fades in over
//! the big window, which then closes, everything turns back, the panel
//! opens underneath, the sheet fades to show it, and the panel slides away.
//!
//! Where things land comes from what each window drew last time (`seen_*`),
//! so the first flip after starting gitstat may land a few px off; the
//! reveal's fade hides that. The big window's frame (how far its client
//! area sits inside the bounds it was opened with, maximised or not) is
//! measured once the window has settled and kept in window.json, so the
//! sheet lands on it from the first flip.
//!
//! Words that differ between the two views (a short and a long checkout,
//! bold and plain) cross-fade over the middle of the turn rather than
//! swapping at once.

use crate::config::Section;
use crate::layout::Rect;
use crate::timeline::{COLUMN_WIDTH, Ends};
use crate::view::{self, Palette, Ui};
use crate::win;
use gpui::{
    App, AppContext, Bounds, Context, Global, Hsla, IntoElement, ParentElement, Pixels, Render,
    Styled, Window, WindowAppearance, WindowBackgroundAppearance, WindowBounds, WindowHandle,
    WindowKind, WindowOptions, canvas, div, point, px, quad, size,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The phases at the default speed, 560 ms in all; `flip_ms` stretches
/// all but the hold, which is the big window's time to draw.
const FADE: Duration = Duration::from_millis(90);
const TURN: Duration = Duration::from_millis(280);
const HOLD: Duration = Duration::from_millis(80);
const REVEAL: Duration = Duration::from_millis(110);
/// The panel's and the sheet's rounded corners, logical px.
const RADIUS: f32 = 8.0;
/// Until the big window has been measured: its title bar's height,
/// logical px.
const CAPTION_GUESS: f32 = 32.0;
/// Room left around the sheet for its shadow, logical px.
const SHADOW_ROOM: f32 = 40.0;
/// Until the big window has been drawn once: its column header's height,
/// and the table's top and bottom margins, logical px.
const HEADER_GUESS: f32 = 110.0;
const TABLE_TOP_GUESS: f32 = 60.0;
const TABLE_BOTTOM_GUESS: f32 = 40.0;

/// A piece of a repo's row that its column header shows too, and the
/// flip carries from one to the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    Icon,
    Name,
    Checkout,
    Status,
    /// The top bar both views have, and what's on it: the headline, the
    /// line below it, and the settings and refresh buttons. Kept under an
    /// empty path.
    Bar,
    Headline,
    Subline,
    Settings,
    Refresh,
}

/// The path the top bar's parts are kept under.
pub fn app() -> &'static Path {
    Path::new("")
}

/// A part as one view last drew it: its bounds in that window's logical px
/// and, for words, the words and how they look.
#[derive(Clone, Debug)]
struct Seen {
    bounds: Bounds<Pixels>,
    text: Option<Words>,
}

/// Where one view draws a part.
#[derive(Clone, Copy)]
pub enum Side {
    Panel,
    Column,
}

/// How a part's words look: size in px and whether bold.
pub struct Look {
    pub color: Hsla,
    pub size: f32,
    pub bold: bool,
}

thread_local! {
    /// The panel's parts, by repo, as last drawn: panel logical px.
    static PANEL_PARTS: RefCell<HashMap<(PathBuf, Part), Seen>> = RefCell::new(HashMap::new());
    /// The big window's header parts, by repo, as last drawn: window
    /// logical px, beside each header's own top-left corner.
    static COLUMN_PARTS: RefCell<HashMap<(PathBuf, Part), Seen>> = RefCell::new(HashMap::new());
    /// The panel headings' words: bounds, text and colour.
    static HEADING_LABELS: RefCell<HashMap<SectionKey, HeadingLabel>> =
        RefCell::new(HashMap::new());
    /// The spines' names: top and height in the big window's logical px,
    /// and text.
    static SPINE_LABELS: RefCell<HashMap<SectionKey, (f32, f32, String)>> =
        RefCell::new(HashMap::new());
    static HEADER_AT: RefCell<HashMap<PathBuf, gpui::Point<Pixels>>> = RefCell::new(HashMap::new());
    /// Each strip in view, by repo, as the panel last drew it: bounds in
    /// the panel's logical px.
    static STRIPS: RefCell<HashMap<PathBuf, Bounds<Pixels>>> = RefCell::new(HashMap::new());
    /// Each section heading in view, as the panel last drew it.
    static HEADERS: RefCell<HashMap<SectionKey, Bounds<Pixels>>> = RefCell::new(HashMap::new());
    /// The column body's top below the big window's client top, physical
    /// px, as last drawn.
    static BODY_TOP: Cell<Option<f32>> = const { Cell::new(None) };
    /// A spine's top, and its bottom's distance above the client bottom,
    /// physical px, as last drawn.
    static SPINE: Cell<Option<(f32, f32)>> = const { Cell::new(None) };
    /// The big window's client area minus the bounds it was opened with,
    /// physical px per edge (left, top, right, bottom), once measured.
    static CLIENT_DELTA: Cell<Option<Delta>> = const { Cell::new(None) };
    /// The same for a maximised big window, against its monitor's work area.
    static MAXIMIZED_DELTA: Cell<Option<Delta>> = const { Cell::new(None) };
    /// The big window's title bar height, physical px, normal and
    /// maximised, once measured.
    static CAPTION: Cell<Option<i32>> = const { Cell::new(None) };
    static MAXIMIZED_CAPTION: Cell<Option<i32>> = const { Cell::new(None) };
    /// When the big window last reached the place and state it opened
    /// with (a maximised one shows at its normal size first).
    static SETTLED: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// A panel heading's words as drawn: bounds, text and colour.
type HeadingLabel = (Bounds<Pixels>, String, Hsla);

/// `Section` as a map key.
type SectionKey = u8;

fn key(section: Section) -> SectionKey {
    match section {
        Section::NeedsYou => 0,
        Section::Synced => 1,
        Section::PullRequests => 2,
        Section::GithubOnly => 3,
    }
}

/// Notes a strip the panel drew, for the flip.
pub fn seen_strip(path: &Path, bounds: Bounds<Pixels>) {
    STRIPS.with(|s| {
        s.borrow_mut().insert(path.to_path_buf(), bounds);
    });
}

/// An invisible layer, absolutely over its parent (which must be
/// `relative`), that notes where `side` drew `path`'s `part`, and its words
/// in `look` when it has words.
pub fn tag(
    side: Side,
    path: &Path,
    part: Part,
    words: Option<(gpui::SharedString, Look)>,
) -> impl IntoElement {
    let path = path.to_path_buf();
    let text = words.map(|(t, l)| (t, l.color, l.size, l.bold));
    canvas(
        |_, _, _| {},
        move |bounds, _, _, _| {
            if part == Part::Bar {
                LAST_PAINT.with(|t| t.set(Some(Instant::now())));
            }
            let seen = Seen {
                bounds,
                text: text.clone(),
            };
            let map = match side {
                Side::Panel => &PANEL_PARTS,
                Side::Column => &COLUMN_PARTS,
            };
            map.with(|m| {
                m.borrow_mut().insert((path.clone(), part), seen);
            });
        },
    )
    .absolute()
    .inset_0()
}

/// An invisible layer over a column header noting where its top-left
/// corner is, which its parts are placed beside.
pub fn tag_header(path: &Path) -> impl IntoElement {
    let path = path.to_path_buf();
    canvas(
        |_, _, _| {},
        move |bounds, _, _, _| {
            HEADER_AT.with(|m| {
                m.borrow_mut().insert(path.clone(), bounds.origin);
            });
        },
    )
    .absolute()
    .inset_0()
}

/// Notes the words of a panel section heading, for the flip.
pub fn seen_heading_label(section: Section, bounds: Bounds<Pixels>, text: &str, color: Hsla) {
    HEADING_LABELS.with(|m| {
        m.borrow_mut()
            .insert(key(section), (bounds, text.to_string(), color));
    });
}

/// Notes a spine's name as the big window drew it, for the flip.
pub fn seen_spine_label(section: Section, bounds: Bounds<Pixels>, text: &str, _: &Window) {
    SPINE_LABELS.with(|m| {
        m.borrow_mut().insert(
            key(section),
            (
                f32::from(bounds.top()),
                f32::from(bounds.size.height),
                text.to_string(),
            ),
        );
    });
}

/// Notes a section heading the panel drew, for the flip.
pub fn seen_header(section: Section, bounds: Bounds<Pixels>) {
    HEADERS.with(|h| {
        h.borrow_mut().insert(key(section), bounds);
    });
}

/// Forgets the strips and headings; the panel is opening afresh.
pub fn clear_strips() {
    STRIPS.with(|s| s.borrow_mut().clear());
    PANEL_PARTS.with(|s| s.borrow_mut().clear());
    HEADING_LABELS.with(|s| s.borrow_mut().clear());
    HEADERS.with(|h| h.borrow_mut().clear());
}

/// Notes a column body the big window drew, for the flip.
pub fn seen_column(body: Bounds<Pixels>, window: &Window) {
    let top = f32::from(body.top()) * window.scale_factor();
    BODY_TOP.with(|t| t.set(Some(top)));
}

/// Notes a section spine the big window drew, for the flip.
pub fn seen_spine(spine: Bounds<Pixels>, window: &Window) {
    let s = window.scale_factor();
    let below = f32::from(window.viewport_size().height - spine.bottom()) * s;
    SPINE.with(|t| t.set(Some((f32::from(spine.top()) * s, below))));
}

/// A client area minus the bounds asked for, physical px per edge (left,
/// top, right, bottom).
pub type Delta = (i32, i32, i32, i32);

/// Notes where the big window's client area is for the bounds it was
/// opened with (or, maximised, its monitor's work area), both physical px.
/// Only once the window has settled into that state: a maximised window
/// measured before it maximises would send the flip to its normal size.
/// With its title bar's height, physical px, when known.
pub fn seen_client(requested: Rect, client: Rect, maximized: bool, caption: Option<i32>) {
    let delta = if maximized {
        &MAXIMIZED_DELTA
    } else {
        &CLIENT_DELTA
    };
    if caption.is_some() {
        let cell = if maximized {
            &MAXIMIZED_CAPTION
        } else {
            &CAPTION
        };
        cell.with(|c| c.set(caption));
    }
    let d = (
        client.left - requested.left,
        client.top - requested.top,
        client.right - requested.right,
        client.bottom - requested.bottom,
    );
    // A frame is a few dozen px at most; anything more is a window caught
    // between states.
    let sane = [d.0, d.1, d.2, d.3].iter().all(|v| v.abs() <= 120);
    if sane {
        delta.with(|c| c.set(Some(d)));
    }
}

/// Both frames as measured, (normal, maximised), for window.json.
pub fn deltas() -> (Option<Delta>, Option<Delta>) {
    (
        CLIENT_DELTA.with(Cell::get),
        MAXIMIZED_DELTA.with(Cell::get),
    )
}

/// Both title bar heights as measured, (normal, maximised), for window.json.
pub fn captions() -> (Option<i32>, Option<i32>) {
    (CAPTION.with(Cell::get), MAXIMIZED_CAPTION.with(Cell::get))
}

/// Title bar heights kept from an earlier run, where none is measured yet.
pub fn learn_captions(normal: Option<i32>, maximized: Option<i32>) {
    for (cell, kept) in [(&CAPTION, normal), (&MAXIMIZED_CAPTION, maximized)] {
        cell.with(|c| {
            if c.get().is_none() {
                c.set(kept);
            }
        });
    }
}

/// Frames kept from an earlier run, where none is measured yet this run.
pub fn learn_deltas(normal: Option<Delta>, maximized: Option<Delta>) {
    for (cell, kept) in [(&CLIENT_DELTA, normal), (&MAXIMIZED_DELTA, maximized)] {
        cell.with(|c| {
            if c.get().is_none() {
                c.set(kept);
            }
        });
    }
}

/// The big window has reached the place and state it was opened with.
pub fn big_settled() {
    SETTLED.with(|s| s.set(Some(Instant::now())));
}

#[derive(Default)]
struct MorphSlot {
    handle: Option<WindowHandle<MorphView>>,
    /// Its HWND, which a window opening under it is placed below.
    hwnd: Option<isize>,
    /// When it opened.
    since: Option<Instant>,
}

impl Global for MorphSlot {}

/// Whether a flip is drawing now.
pub fn running(cx: &App) -> bool {
    cx.try_global::<MorphSlot>().is_some_and(|s| {
        s.handle.is_some_and(|h| view::is_live(h.into(), cx))
            && s.since.is_some_and(|t| t.elapsed() < STUCK)
    })
}

/// The flip's HWND while it draws.
pub fn flip_hwnd(cx: &App) -> Option<isize> {
    if !running(cx) {
        return None;
    }
    cx.try_global::<MorphSlot>()?.hwnd
}

/// Longer than any flip runs: one still around after this is stuck, and
/// no longer holds up the windows.
const STUCK: Duration = Duration::from_secs(8);

fn enabled(cx: &App) -> bool {
    cx.try_global::<Ui>().is_some_and(|ui| {
        let cfg = &ui.state.read(cx).config;
        cfg.flip_animation && cfg.window && cfg.panel
    })
}

/// Opens the big window from the panel, scrolled to `focus` if given, with
/// the flip when it's on. Called from the panel's own handlers; closes the
/// panel either way.
pub fn open_window(panel: &mut Window, focus: Option<PathBuf>, cx: &mut App) {
    if let Some(path) = &focus
        && let Some(t) = cx.try_global::<Ui>().map(|ui| ui.timelines.clone())
    {
        t.update(cx, |t, _| t.focus(path));
    }
    if running(cx) {
        return;
    }
    let flip = enabled(cx) && !view::window_open(cx);
    let started = flip
        && win::hwnd(panel)
            .and_then(win::client_rect)
            .and_then(|rect| {
                let plan = plan(rect, panel.scale_factor(), None, focus.as_deref(), cx)?;
                Some(launch(plan, Direction::Open, cx))
            })
            .unwrap_or(false);
    if !started {
        crate::panel::dismiss(panel, cx);
        view::show_window(cx);
    }
}

/// Opens the big window from the tray or the badge: through the panel at
/// `anchor` when the flip is on, else straight away.
pub fn show_window(anchor: crate::panel::Anchor, cx: &mut App) {
    if !enabled(cx) || view::window_open(cx) || running(cx) {
        if !view::window_open(cx) {
            crate::panel::close(cx);
        }
        view::show_window(cx);
        return;
    }
    crate::panel::then_window(anchor, cx);
}

/// Closing the big window: starts the flip back into the panel (which then
/// slides away, or stays when the window was minimised) and returns
/// true, leaving the window for the flip to close; false when the flip is
/// off or can't run, so the caller closes it.
pub fn flip_back(big: &mut Window, cx: &mut App) -> bool {
    let minimized = win::take_minimized();
    // Opens there again next time.
    view::remember_place(big, cx);
    if running(cx) {
        // Mid-flip; the flip closes it.
        return true;
    }
    if !enabled(cx) {
        return false;
    }
    let Some(panel_rect) = crate::panel::last_rect(cx) else {
        return false;
    };
    let Some(big_rect) = win::hwnd(big).and_then(win::client_rect) else {
        return false;
    };
    // The panel draws at the primary monitor's scale, like the big window.
    let ps = big.scale_factor();
    let scroll = view::table_scroll(cx) * ps;
    let Some(plan) = plan(panel_rect, ps, Some((big_rect, scroll)), None, cx) else {
        return false;
    };
    let direction = if minimized {
        Direction::Minimize
    } else {
        Direction::Close
    };
    launch(plan, direction, cx)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// Panel to big window.
    Open,
    /// Big window to panel, which then slides away.
    Close,
    /// Big window to panel, which stays.
    Minimize,
}

/// One repo's tree in the flip.
struct Tree {
    graph: Arc<crate::repo_graph::RepoGraph>,
    dirty: bool,
    /// Physical px, from the flip window's top-left corner.
    ends: Ends,
    /// Its column is past the big window's edge: it fades as it turns.
    gone: bool,
}

/// A section's heading in the panel and its spine in the big window,
/// physical px from the flip window's top-left corner.
struct Spine {
    section: Section,
    from: Bounds<f32>,
    to: Bounds<f32>,
    name: Option<Name>,
}

/// A section's name, from the panel heading's words to the spine's, which
/// read upwards: centres and sizes in physical px, and the words at each end.
struct Name {
    from: (f32, f32),
    to: (f32, f32),
    from_size: f32,
    to_size: f32,
    from_text: String,
    to_text: String,
    color: Hsla,
}

/// A part's words: text, colour, size in px, and whether bold.
type Words = (gpui::SharedString, Hsla, f32, bool);

/// A carried part this frame: where, its picture, its words (two while
/// they cross-fade), alpha, and whether it is the top bar.
type Drawn = (
    Bounds<Pixels>,
    Option<Arc<gpui::RenderImage>>,
    Vec<Words>,
    f32,
    bool,
);

/// A part carried from a panel row to a column header, physical px from
/// the flip window's top-left corner.
struct Carried {
    from: Bounds<f32>,
    to: Bounds<f32>,
    /// The words at each end: text, colour, size and boldness.
    words: Option<(Words, Words)>,
    /// For the icon: its tone, to find its picture.
    status: Option<crate::git::RepoStatus>,
    gone: bool,
    /// The top bar itself: its bottom edge is drawn.
    bar: bool,
}

/// Everything a flip draws, laid out.
struct Plan {
    /// The flip window's place, physical px.
    union: Rect,
    /// The panel's and the big window's client areas, physical px from the
    /// flip window's top-left corner.
    from: Bounds<f32>,
    to: Bounds<f32>,
    /// The big window's title bar height, physical px: the sheet grows
    /// over it too, so the bar never pops in at the end.
    caption: f32,
    /// The big window is maximised (or will be): it has no rim.
    maximized: bool,
    /// The work areas the flip may draw in, physical px from its top-left
    /// corner: never over a taskbar, which the windows' own shadows stay
    /// under.
    work: Bounds<f32>,
    trees: Vec<Tree>,
    spines: Vec<Spine>,
    parts: Vec<Carried>,
}

/// Lays out a flip between the panel at `panel_rect` (client area,
/// physical px, drawn at `ps`) and the big window: at `big`'s client rect
/// with its table scrolled that far (physical px), or where show_window
/// will open it, scrolled to `focus`. None when nothing would turn.
fn plan(
    panel_rect: Rect,
    ps: f32,
    big: Option<(Rect, f32)>,
    focus: Option<&Path>,
    cx: &App,
) -> Option<Plan> {
    let ui = cx.try_global::<Ui>()?;
    let state = ui.state.read(cx);
    let timelines = ui.timelines.read(cx);
    let strips = STRIPS.with(|s| s.borrow().clone());
    let headers = HEADERS.with(|h| h.borrow().clone());

    let cells = view::table_cells(state);
    let lefts = view::cell_lefts(&cells);
    let repos = cells
        .iter()
        .filter(|c| matches!(c, view::Cell::Repo(_)))
        .count();
    let (target, ws) = match big {
        Some((rect, _)) => (rect, ps),
        None => {
            // Where show_window will put it.
            let (place, ws) = view::window_place(repos, cx);
            let bounds = place.get_bounds();
            let at = |v: Pixels| (f32::from(v) * ws).round() as i32;
            let (area, delta) = match place {
                WindowBounds::Maximized(_) => (
                    view::maximized_area(bounds, ws),
                    MAXIMIZED_DELTA.with(Cell::get).unwrap_or_default(),
                ),
                _ => (
                    Rect::new(
                        at(bounds.origin.x),
                        at(bounds.origin.y),
                        at(bounds.origin.x + bounds.size.width),
                        at(bounds.origin.y + bounds.size.height),
                    ),
                    CLIENT_DELTA.with(Cell::get).unwrap_or_default(),
                ),
            };
            let (dl, dt, dr, db) = delta;
            (
                Rect::new(
                    area.left + dl,
                    area.top + dt,
                    area.right + dr,
                    area.bottom + db,
                ),
                ws,
            )
        }
    };

    // The big window's title bar, above its client area.
    let maximized = matches!(view::window_place(0, cx).0, WindowBounds::Maximized(_));
    let caption = if maximized {
        MAXIMIZED_CAPTION.with(Cell::get)
    } else {
        CAPTION.with(Cell::get)
    }
    .unwrap_or((CAPTION_GUESS * ws).round() as i32);
    // Room around both for the sheet's shadow.
    let m = (SHADOW_ROOM * ps).ceil() as i32;
    let union = Rect::new(
        panel_rect.left.min(target.left) - m,
        panel_rect.top.min(target.top - caption) - m,
        panel_rect.right.max(target.right) + m,
        panel_rect.bottom.max(target.bottom) + m,
    );
    let local = |r: Rect| Bounds {
        origin: point((r.left - union.left) as f32, (r.top - union.top) as f32),
        size: size(r.width() as f32, r.height() as f32),
    };
    let (from, to) = (local(panel_rect), local(target));
    let work = {
        let (a, b) = (panel_rect.centre(), target.centre());
        let (a, b) = (
            win::monitor_at(a.0, a.1).work,
            win::monitor_at(b.0, b.1).work,
        );
        local(Rect::new(
            a.left.min(b.left),
            a.top.min(b.top),
            a.right.max(b.right),
            a.bottom.max(b.bottom),
        ))
    };
    // In the panel, logical px to the flip window's physical px.
    let in_panel = |b: &Bounds<Pixels>| Bounds {
        origin: point(
            from.origin.x + f32::from(b.left()) * ps,
            from.origin.y + f32::from(b.top()) * ps,
        ),
        size: size(f32::from(b.size.width) * ps, f32::from(b.size.height) * ps),
    };

    // The table's scroll: as it is, or bringing the focused column in.
    let scroll = match big {
        Some((_, scroll)) => scroll,
        None => focus
            .and_then(|f| view::cell_of(&cells, f))
            .map_or(0.0, |i| {
                ((lefts[i] + COLUMN_WIDTH) * ws - to.size.width).max(0.0)
            }),
    };
    let body_top = BODY_TOP.with(Cell::get).unwrap_or(HEADER_GUESS * ws);
    let (spine_top, spine_below) = SPINE
        .with(Cell::get)
        .unwrap_or((TABLE_TOP_GUESS * ws, TABLE_BOTTOM_GUESS * ws));

    let mut trees = Vec::new();
    let mut spines = Vec::new();
    let mut parts = Vec::new();
    let panel_parts = PANEL_PARTS.with(|m| m.borrow().clone());
    let column_parts = COLUMN_PARTS.with(|m| m.borrow().clone());
    let header_at = HEADER_AT.with(|m| m.borrow().clone());
    for (cell, left) in cells.iter().zip(&lefts) {
        let x = to.origin.x + left * ws - scroll;
        let gone = x + cell.width() * ws <= to.origin.x || x >= to.origin.x + to.size.width;
        match *cell {
            view::Cell::Repo(status) => {
                let (Some(strip), Some(graph)) =
                    (strips.get(&status.path), timelines.graph_of(&status.path))
                else {
                    continue;
                };
                // The parts both views show, where each last drew them.
                if let Some(hat) = header_at.get(&status.path) {
                    for part in [Part::Icon, Part::Name, Part::Checkout, Part::Status] {
                        let key = (status.path.clone(), part);
                        let (Some(a), Some(b)) = (panel_parts.get(&key), column_parts.get(&key))
                        else {
                            continue;
                        };
                        let dx = f32::from(b.bounds.origin.x - hat.x) * ws;
                        let to_b = Bounds {
                            origin: point(x + dx, to.origin.y + f32::from(b.bounds.origin.y) * ws),
                            size: size(
                                f32::from(b.bounds.size.width) * ws,
                                f32::from(b.bounds.size.height) * ws,
                            ),
                        };
                        let words = a.text.clone().zip(b.text.clone());
                        if part != Part::Icon && words.is_none() {
                            continue;
                        }
                        parts.push(Carried {
                            from: in_panel(&a.bounds),
                            to: to_b,
                            words,
                            status: (part == Part::Icon).then(|| status.clone()),
                            gone,
                            bar: false,
                        });
                    }
                }
                let strip = in_panel(strip);
                let newest = strip.origin.x + strip.size.width - crate::timeline::STRIP_PAD * ps;
                trees.push(Tree {
                    graph,
                    dirty: status.dirty,
                    ends: Ends {
                        strip: (newest, strip.origin.y + strip.size.height / 2.0),
                        strip_len: newest - strip.origin.x,
                        column: (x, to.origin.y + body_top),
                        column_len: (to.size.height - body_top).max(0.0),
                    },
                    gone,
                });
            }
            view::Cell::Spine(section, _) => {
                let Some(header) = headers.get(&key(section)) else {
                    continue;
                };
                if gone {
                    continue;
                }
                let k = key(section);
                let name = HEADING_LABELS
                    .with(|m| m.borrow().get(&k).cloned())
                    .zip(SPINE_LABELS.with(|m| m.borrow().get(&k).cloned()))
                    .map(|((lb, from_text, color), (top, height, to_text))| {
                        let from_size = 14.0;
                        let len = crate::turned_text::length(&from_text, from_size);
                        let lb = in_panel(&lb);
                        Name {
                            from: (
                                lb.origin.x + len * ps / 2.0,
                                lb.origin.y + lb.size.height / 2.0,
                            ),
                            to: (
                                x + view::SPINE_WIDTH * ws / 2.0,
                                to.origin.y + (top + height / 2.0) * ws,
                            ),
                            from_size: from_size * ps,
                            to_size: view::SPINE_TEXT * ws,
                            from_text,
                            to_text,
                            color,
                        }
                    });
                spines.push(Spine {
                    section,
                    name,
                    from: in_panel(header),
                    to: Bounds {
                        origin: point(x, to.origin.y + spine_top),
                        size: size(
                            view::SPINE_WIDTH * ws,
                            (to.size.height - spine_top - spine_below).max(0.0),
                        ),
                    },
                });
            }
        }
    }
    // The top bar and what's on it, from the panel's to the window's.
    for part in [
        Part::Bar,
        Part::Headline,
        Part::Subline,
        Part::Settings,
        Part::Refresh,
    ] {
        let key = (app().to_path_buf(), part);
        let (Some(a), Some(b)) = (panel_parts.get(&key), column_parts.get(&key)) else {
            continue;
        };
        let words = a.text.clone().zip(b.text.clone());
        if part != Part::Bar && words.is_none() {
            continue;
        }
        parts.push(Carried {
            from: in_panel(&a.bounds),
            to: Bounds {
                origin: point(
                    to.origin.x + f32::from(b.bounds.origin.x) * ws,
                    to.origin.y + f32::from(b.bounds.origin.y) * ws,
                ),
                size: size(
                    f32::from(b.bounds.size.width) * ws,
                    f32::from(b.bounds.size.height) * ws,
                ),
            },
            words,
            status: None,
            gone: false,
            bar: part == Part::Bar,
        });
    }
    (!trees.is_empty() || !spines.is_empty()).then_some(Plan {
        union,
        from,
        to,
        caption: caption as f32,
        maximized,
        work,
        trees,
        spines,
        parts,
    })
}

/// Opens the flip's window for `plan`. False if it couldn't.
fn launch(plan: Plan, direction: Direction, cx: &mut App) -> bool {
    let union = plan.union;
    let ms = cx
        .try_global::<Ui>()
        .map_or(560, |ui| ui.state.read(cx).config.flip_ms);
    let fixed = HOLD.as_millis() as f32;
    let stretch = ((ms as f32 - fixed) / (560.0 - fixed)).clamp(0.2, 6.0);
    let mut flip_hwnd = None;
    let Ok(handle) = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(crate::panel::logical(union))),
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
            flip_hwnd = win::hwnd(window);
            if let Some(h) = flip_hwnd {
                // Shown a moment after it opens, from a timer: a hidden
                // window may draw no frames, so it can't show itself.
                cx.spawn(async move |cx| {
                    unsafe { win::style_morph(h) };
                    cx.background_executor()
                        .timer(Duration::from_millis(20))
                        .await;
                    unsafe { win::place_morph(h, union) };
                })
                .detach();
            }
            cx.new(|_| MorphView {
                start: Instant::now(),
                stretch,
                plan,
                direction,
                hwnd: win::hwnd(window),
                left: false,
                arrived: None,
                reveal_from: None,
                frames: 0,
                framed: (u32::MAX, u32::MAX),
                hiding: false,
            })
        },
    ) else {
        return false;
    };
    // A stuck flip from before goes.
    let old = cx.default_global::<MorphSlot>().handle.replace(handle);
    if let Some(old) = old {
        old.update(cx, |_, window, _| window.remove_window()).ok();
    }
    let slot = cx.default_global::<MorphSlot>();
    slot.since = Some(Instant::now());
    slot.hwnd = flip_hwnd;
    true
}

pub struct MorphView {
    start: Instant,
    /// `flip_ms` over the default.
    stretch: f32,
    plan: Plan,
    direction: Direction,
    hwnd: Option<isize>,
    /// The window the flip starts from has closed.
    left: bool,
    /// When the window it ends in was opened.
    arrived: Option<Instant>,
    /// When the reveal began: once that window has drawn.
    reveal_from: Option<Instant>,
    /// Frames drawn; shown from the second.
    frames: u32,
    /// The frames between which the sheet draws the windows' rim and
    /// shadow: from the one after the starting window closed until the one
    /// after the ending window showed, so the real and the drawn one hand
    /// over without both showing, or neither.
    framed: (u32, u32),
    /// Hidden, to be removed next frame.
    hiding: bool,
}

thread_local! {
    /// When a panel or big window last drew its top bar.
    static LAST_PAINT: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// The longest the flip waits for the window it ends in to draw.
const DRAW_WAIT: Duration = Duration::from_millis(500);

/// `t` eased in and out.
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// How far `elapsed` is into the phase that starts `at` and lasts `len`.
fn phase(elapsed: Duration, at: Duration, len: Duration) -> f32 {
    (elapsed.saturating_sub(at).as_secs_f32() / len.as_secs_f32()).clamp(0.0, 1.0)
}

fn lerp_bounds(a: Bounds<f32>, b: Bounds<f32>, e: f32, s: f32) -> Bounds<Pixels> {
    let l = |a: f32, b: f32| (a + (b - a) * e) / s;
    Bounds::new(
        point(px(l(a.origin.x, b.origin.x)), px(l(a.origin.y, b.origin.y))),
        size(
            px(l(a.size.width, b.size.width)),
            px(l(a.size.height, b.size.height)),
        ),
    )
}

impl Render for MorphView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.hiding {
            window.remove_window();
            return div();
        }
        // Shown once its first frame is drawn; the clock starts then.
        self.frames += 1;
        if self.frames == 2 {
            self.start = Instant::now();
        }
        let elapsed = if self.frames < 2 {
            Duration::ZERO
        } else {
            self.start.elapsed()
        };
        let k = self.stretch;
        let (fade, turn, reveal_len) = (FADE.mul_f32(k), TURN.mul_f32(k), REVEAL.mul_f32(k));
        let turn_at = fade;
        let hold_at = turn_at + turn;
        let open = self.direction == Direction::Open;
        // The reveal waits for the window underneath to have drawn, so it
        // never shows that window empty.
        if self.reveal_from.is_none()
            && let Some(arrived) = self.arrived
        {
            let since = arrived.elapsed();
            // Opening, the big window must also have settled (a maximised
            // one maximises a moment after it shows) and drawn since.
            let ready = if open {
                SETTLED.with(Cell::get).filter(|&t| t >= arrived)
            } else {
                Some(arrived)
            };
            let drawn = ready
                .zip(LAST_PAINT.with(Cell::get))
                .is_some_and(|(r, t)| t > r.max(arrived + Duration::from_millis(30)));
            if since >= DRAW_WAIT || (since >= HOLD && drawn) {
                self.reveal_from = Some(Instant::now());
            }
        }
        let revealed = self.reveal_from.map_or(Duration::ZERO, |t| t.elapsed());

        if elapsed >= turn_at && !self.left {
            self.left = true;
            self.framed.0 = self.frames + 1;
            if open {
                cx.defer(crate::panel::close);
            } else {
                cx.defer(view::close_window);
            }
        }
        if elapsed >= hold_at && self.arrived.is_none() {
            self.arrived = Some(Instant::now());
            self.framed.1 = self.frames + 1;
            if open {
                cx.defer(view::show_window);
            } else {
                let me = self.hwnd;
                cx.defer(move |cx| {
                    crate::panel::reopen(cx);
                    // The panel goes topmost as it opens; the flip stays
                    // over it until the reveal.
                    if let Some(h) = me {
                        cx.spawn(async move |_| unsafe { win::reassert_topmost(h) })
                            .detach();
                    }
                });
            }
        }
        if self.reveal_from.is_some() && revealed >= reveal_len {
            // Hidden first and removed a moment later, so its last frame
            // never lingers as a flash. Removed from a timer: a hidden
            // window draws no more frames, so it can't remove itself.
            self.hiding = true;
            let me = window.window_handle();
            let hwnd = self.hwnd;
            cx.spawn(async move |_, cx| {
                if let Some(h) = hwnd {
                    unsafe { win::hide(h) };
                }
                cx.background_executor()
                    .timer(Duration::from_millis(40))
                    .await;
                cx.update_window(me, |_, window, _| window.remove_window())
                    .ok();
            })
            .detach();
            if self.direction == Direction::Close {
                // Back in the panel, which then goes the way it came.
                cx.defer(crate::panel::slide_away);
            }
            window.request_animation_frame();
            return div();
        }

        let sheet_alpha = ease(phase(elapsed, Duration::ZERO, fade));
        let turned = ease(phase(elapsed, turn_at, turn));
        // 0 is the panel's layout, 1 the big window's.
        let e = if open { turned } else { 1.0 - turned };
        let reveal = 1.0 - ease(phase(revealed, Duration::ZERO, reveal_len));
        // The windows' own shadow, drawn around the sheet while neither
        // window is on screen to show its own.
        let framed = if (self.framed.0..self.framed.1).contains(&self.frames) {
            1.0
        } else {
            0.0
        };
        let appearance = window.appearance();
        let dark = matches!(
            appearance,
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let p = Palette::for_appearance(appearance);
        let s = window.scale_factor();
        // The sheet grows over the title bar too; that part takes its
        // colour, as wide as the sheet and as tall as the turn has gone.
        let cap = self.plan.caption;
        let to_sheet = Bounds {
            origin: point(self.plan.to.origin.x, self.plan.to.origin.y - cap),
            size: size(self.plan.to.size.width, self.plan.to.size.height + cap),
        };
        let sheet = lerp_bounds(self.plan.from, to_sheet, e, s);
        let work = lerp_bounds(self.plan.work, self.plan.work, 0.0, s);
        let title = Bounds::new(sheet.origin, size(sheet.size.width, px(cap * e / s)));
        let title_color = view::title_bar(dark);
        // The rim stays drawn from when the starting window has gone to the
        // very end, over the ending window's own of the same colour, so it
        // never blinks. A maximised big window has none: there it thins
        // out toward that end.
        let rim_alpha = if self.frames >= self.framed.0 {
            if self.plan.maximized { 1.0 - e } else { 1.0 }
        } else {
            0.0
        };
        let trees: Vec<(Arc<crate::repo_graph::RepoGraph>, bool, Ends, f32)> = self
            .plan
            .trees
            .iter()
            .map(|t| {
                let k = |(x, y): (f32, f32)| (x / s, y / s);
                let ends = Ends {
                    strip: k(t.ends.strip),
                    strip_len: t.ends.strip_len / s,
                    column: k(t.ends.column),
                    column_len: t.ends.column_len / s,
                };
                let alpha = if t.gone { 1.0 - e } else { 1.0 };
                (t.graph.clone(), t.dirty, ends, alpha * reveal)
            })
            .collect();
        // Each bar is drawn solid, its wash already laid on the panel's
        // colour, so it never shows through to the bars beneath it.
        // Words that differ cross-fade over the turn's middle.
        let blend = ease((e - 0.3) / 0.4);
        type Named = (gpui::Point<Pixels>, Vec<(String, f32)>, f32, Hsla);
        let spines: Vec<(Bounds<Pixels>, Hsla, Option<Named>)> = self
            .plan
            .spines
            .iter()
            .map(|sp| {
                let mix = |a: f32, b: f32| a + (b - a) * e;
                let name = sp.name.as_ref().map(|n| {
                    (
                        point(px(mix(n.from.0, n.to.0) / s), px(mix(n.from.1, n.to.1) / s)),
                        if n.from_text == n.to_text {
                            vec![(n.from_text.clone(), 1.0)]
                        } else {
                            vec![
                                (n.from_text.clone(), 1.0 - blend),
                                (n.to_text.clone(), blend),
                            ]
                        },
                        mix(n.from_size, n.to_size) / s,
                        n.color,
                    )
                });
                (
                    lerp_bounds(sp.from, sp.to, e, s),
                    p.surface.blend(view::section_tint(sp.section, &p)),
                    name,
                )
            })
            .collect();

        // The parts both views show, moving and growing from one to the
        // other, over the sheet: they never fade with the words.
        let parts: Vec<Drawn> = self
            .plan
            .parts
            .iter()
            .map(|c| {
                let b = lerp_bounds(c.from, c.to, e, s);
                let alpha = reveal * if c.gone { 1.0 - e } else { 1.0 };
                let image = c.status.as_ref().and_then(|st| {
                    let a = c.from.size.width.round() as u32;
                    let z = c.to.size.width.round() as u32;
                    crate::repo_marks::picture(st, &[a.max(z), a.min(z)], cx)
                });
                let words = c.words.as_ref().map_or_else(Vec::new, |(a, z)| {
                    let mix = |x: f32, y: f32| x + (y - x) * e;
                    let size = mix(a.2, z.2);
                    if a.0 == z.0 && a.3 == z.3 {
                        // The same words: one line, its colour moving.
                        let color = Hsla {
                            h: if e < 0.5 { a.1.h } else { z.1.h },
                            s: mix(a.1.s, z.1.s),
                            l: mix(a.1.l, z.1.l),
                            a: mix(a.1.a, z.1.a),
                        };
                        vec![(a.0.clone(), color, size, a.3)]
                    } else {
                        // Different words: each in its own colour, one
                        // fading out as the other fades in.
                        let fade = |c: Hsla, k: f32| Hsla { a: c.a * k, ..c };
                        vec![
                            (a.0.clone(), fade(a.1, 1.0 - blend), size, a.3),
                            (z.0.clone(), fade(z.1, blend), size, z.3),
                        ]
                    }
                });
                (b, image, words, alpha, c.bar)
            })
            .collect();

        let painter = canvas(
            |_, _, _| {},
            move |_, _, window, cx| {
                // Never over a taskbar.
                window.with_content_mask(Some(gpui::ContentMask { bounds: work }), |window| {
                    window.request_animation_frame();
                    let bg = p.surface.opacity(sheet_alpha * reveal);
                    if framed > 0.0 {
                        window.paint_shadows(
                            sheet,
                            gpui::Corners::all(px(RADIUS)),
                            &[
                                gpui::BoxShadow {
                                    color: gpui::black().opacity(0.14 * framed),
                                    offset: point(px(0.0), px(2.0)),
                                    blur_radius: px(4.0),
                                    spread_radius: px(0.0),
                                },
                                gpui::BoxShadow {
                                    color: gpui::black()
                                        .opacity(if dark { 0.40 } else { 0.26 } * framed),
                                    offset: point(px(0.0), px(8.0)),
                                    blur_radius: px(24.0),
                                    spread_radius: px(0.0),
                                },
                            ],
                        );
                    }
                    window.paint_quad(quad(
                        sheet,
                        px(RADIUS),
                        bg,
                        px(0.0),
                        gpui::transparent_black(),
                        gpui::BorderStyle::Solid,
                    ));
                    if title.size.height > px(0.5) {
                        let round = px(RADIUS);
                        window.paint_quad(quad(
                            title,
                            gpui::Corners {
                                top_left: round,
                                top_right: round,
                                bottom_left: px(0.0),
                                bottom_right: px(0.0),
                            },
                            title_color.opacity(sheet_alpha * reveal),
                            px(0.0),
                            gpui::transparent_black(),
                            gpui::BorderStyle::Solid,
                        ));
                    }
                    window.with_content_mask(Some(gpui::ContentMask { bounds: sheet }), |window| {
                        // A section's heading stretching into its spine.
                        for (b, bar, name) in &spines {
                            window.paint_quad(gpui::fill(*b, bar.opacity(reveal)));
                            // The name turns with its bar, from across to
                            // reading upwards.
                            if let Some((at, texts, size, color)) = name {
                                for (text, a) in texts {
                                    if *a <= 0.0 {
                                        continue;
                                    }
                                    crate::turned_text::paint_turning(
                                        window,
                                        *at,
                                        text,
                                        *size,
                                        color.opacity(color.a * reveal * a),
                                        e,
                                    );
                                }
                            }
                        }
                        for (graph, dirty, ends, alpha) in &trees {
                            crate::timeline::paint_tree(
                                graph, *dirty, *ends, e, *alpha, dark, &p, window,
                            );
                        }
                        for (b, image, words, alpha, bar) in &parts {
                            if *alpha <= 0.0 {
                                continue;
                            }
                            if *bar {
                                // The header's own colour, darker than the
                                // sheet's, under the sheet's rounded top.
                                let round = px(RADIUS);
                                window.paint_quad(quad(
                                    *b,
                                    gpui::Corners {
                                        top_left: round,
                                        top_right: round,
                                        bottom_left: px(0.0),
                                        bottom_right: px(0.0),
                                    },
                                    p.bg.opacity(sheet_alpha * reveal),
                                    px(0.0),
                                    gpui::transparent_black(),
                                    gpui::BorderStyle::Solid,
                                ));
                                // Its bottom edge, the line under the header.
                                let edge = Bounds::new(
                                    point(b.origin.x, b.bottom() - px(1.0)),
                                    size(b.size.width, px(1.0)),
                                );
                                window.paint_quad(gpui::fill(edge, p.border.opacity(*alpha)));
                                continue;
                            }
                            if let Some(image) = image {
                                window
                                    .paint_image(*b, Default::default(), image.clone(), 0, false)
                                    .ok();
                            }
                            for (text, color, size, bold) in words {
                                if color.a <= 0.0 {
                                    continue;
                                }
                                let mut font = window.text_style().font();
                                if *bold {
                                    font.weight = gpui::FontWeight::SEMIBOLD;
                                }
                                let run = gpui::TextRun {
                                    len: text.len(),
                                    font,
                                    color: color.opacity(color.a * alpha),
                                    background_color: None,
                                    underline: None,
                                    strikethrough: None,
                                };
                                let line = window.text_system().shape_line(
                                    text.clone(),
                                    px(*size),
                                    &[run],
                                    None,
                                );
                                // Cut where the view would cut it.
                                window.with_content_mask(
                                    Some(gpui::ContentMask { bounds: *b }),
                                    |window| line.paint(b.origin, b.size.height, window, cx).ok(),
                                );
                            }
                        }
                    });
                    // The rim: one physical px, light over dark, dark over light.
                    if rim_alpha > 0.0 {
                        let rim = view::rim(dark).opacity(rim_alpha);
                        window.paint_quad(quad(
                            sheet,
                            px(RADIUS),
                            gpui::transparent_black(),
                            px(1.0 / window.scale_factor()),
                            rim,
                            gpui::BorderStyle::Solid,
                        ));
                    }
                });
            },
        )
        .size_full();
        div().size_full().child(painter)
    }
}
