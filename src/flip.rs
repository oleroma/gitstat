//! The flip between the panel and the big window.
//!
//! The panel's window is see-through and covers its monitor's whole work
//! area, so everything between the two views is drawn in it, by the views'
//! own code (`view::stage`): the panel's picture, the big window's where
//! its window is, and the picture part way between them. Nothing is
//! copied, so nothing can differ: the flip's first frame is the view it
//! starts from and its last the view it ends in, px for px.
//!
//! Only once, at the big window's end, does what is on screen change
//! hands. Opening, the big window is made cloaked as the flip begins, and
//! when the flip's last frame shows it, drawn the same underneath, it is
//! uncloaked and the panel's window cloaked in one step. Closing, the
//! panel's window comes up cloaked, drawing the big window where it is,
//! and takes its place in one step before anything moves; the flip then
//! ends as the panel itself, with no step at all.
//!
//! What moves and what fades never overlap in time: first what only the
//! starting view shows fades out, then what both show travels and turns,
//! then what only the ending view shows fades in.

use crate::layout::Rect;
use crate::view::{self, Ui};
use crate::{panel, win};
use gpui::{App, Global, Window};
use std::cell::Cell;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Of a flip's length: how long what only the panel shows takes to fade,
/// and how long the travel and turn take. What only the big window shows
/// fades over the rest.
const PANEL_FADE: f32 = 0.12;
const TURN: f32 = 0.68;
/// How long the trees take to scroll back to their newest commits before
/// a flip, at the default speed.
const SCROLL_HOME: Duration = Duration::from_millis(260);
/// The default `flip_ms`, which `SCROLL_HOME` is paced for.
const DEFAULT_MS: f32 = 900.0;
/// How long a flip waits for a view to draw before it gives up on it.
const DRAW_WAIT: Duration = Duration::from_millis(500);
/// How long it waits for the big window to open where it belongs.
const OPEN_WAIT: Duration = Duration::from_millis(1500);
/// Longer than any flip runs: one still around after this is stuck, and
/// no longer holds up the windows.
const STUCK: Duration = Duration::from_secs(12);
const POLL: Duration = Duration::from_millis(4);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Panel to big window.
    Open,
    /// Big window to panel, which then slides away.
    Close,
    /// Big window to panel, which stays.
    Minimize,
}

/// Something asked for while a flip runs, done once it has ended: the
/// windows the flip is turning between are its own until then, so nothing
/// opens, closes or moves them under it.
#[derive(Clone)]
enum Pending {
    /// Open the big window (the tray, the badge), through the panel.
    ShowWindow(panel::Anchor),
    /// Open the big window from the panel the flip ends in, at a repo.
    FromPanel(Option<PathBuf>),
    /// Open or close the panel (the tray, the badge).
    TogglePanel(panel::Anchor),
    /// The panel the flip ends in goes again (Esc, a click elsewhere).
    PanelLeaves,
    /// The big window the flip ends in closes again (its close button).
    WindowCloses,
}

/// Where the big window is, for the flip to draw it there.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Target {
    /// Its inside on the screen, physical px.
    rect: Rect,
    maximized: bool,
    /// How far into the flip it became known.
    ready: Duration,
}

struct Run {
    direction: Direction,
    len: Duration,
    /// When its first frame was drawn: the flip's clock starts there, so
    /// that frame is the view it starts from.
    clock: Option<Instant>,
    /// Closing: drawn as the big window, cloaked, until it has taken the
    /// big window's place on the screen.
    held: bool,
    target: Option<Target>,
    /// Frames drawn, and the count at which the view it ends in was
    /// first drawn whole.
    frames: u32,
    landed: Option<u32>,
    since: Instant,
}

#[derive(Default)]
struct Slot {
    run: Option<Run>,
    /// About to start, since then: the views are gliding home, or a
    /// window is coming up.
    waiting: Option<(Instant, Direction)>,
    /// Called off while it waited.
    called_off: bool,
    /// What was asked for meanwhile, in order.
    pending: Vec<Pending>,
}

impl Global for Slot {}

thread_local! {
    /// When a view last painted itself, and how many frames the big
    /// window has painted in its own window.
    static PAINTED: Cell<Option<Instant>> = const { Cell::new(None) };
    static WINDOW_FRAMES: Cell<u64> = const { Cell::new(0) };
}

/// The flip's frame drawn last, as (panel, turn, window) in thousandths;
/// none when no flip is drawing. For the self-test, which reads it beside
/// each picture it takes of the screen.
static DRAWN: std::sync::Mutex<Option<(u16, u16, u16)>> = std::sync::Mutex::new(None);

/// The flip's frame drawn last, if one is drawing.
pub fn drawn() -> Option<(u16, u16, u16)> {
    DRAWN.lock().ok().and_then(|d| *d)
}

/// What the flips did and when, for the self-test's report.
static LOG: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

pub fn note(what: impl AsRef<str>) {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let ms = EPOCH.get_or_init(Instant::now).elapsed().as_millis();
    if let Ok(mut log) = LOG.lock()
        && log.len() < 4000
    {
        log.push(format!("{ms:6} {}", what.as_ref()));
    }
}

/// What the flips did since this was last asked.
pub fn take_log() -> Vec<String> {
    LOG.lock()
        .map(|mut l| std::mem::take(&mut *l))
        .unwrap_or_default()
}

fn set_drawn(pose: Option<&Pose>) {
    if let Ok(mut d) = DRAWN.lock() {
        let k = |v: f32| (v * 1000.0).round() as u16;
        *d = pose.map(|p| (k(p.panel), k(p.turn), k(p.window)));
    }
}

thread_local! {
    /// How long the frame being drawn took to lay out and to paint, ms.
    static TOOK: Cell<(f32, f32)> = const { Cell::new((0.0, 0.0)) };
}

/// The flip's frame took this long to lay out and to paint: for its log.
pub fn took(lay: Duration, paint: Duration) {
    TOOK.set((lay.as_secs_f32() * 1000.0, paint.as_secs_f32() * 1000.0));
}

/// A view has painted itself.
pub fn painted() {
    PAINTED.set(Some(Instant::now()));
}

/// The big window has painted a frame in its own window.
pub fn window_drew() {
    WINDOW_FRAMES.set(WINDOW_FRAMES.get() + 1);
}

/// `t` eased in and out.
pub fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// Where a flip is, for the panel's window to draw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    /// How strongly what only the panel shows is drawn, 0 to 1.
    pub panel: f32,
    /// How far what both show has travelled: 0 the panel, 1 the window.
    pub turn: f32,
    /// How strongly what only the big window shows is drawn.
    pub window: f32,
    /// The big window's inside in the panel's window, device px, and
    /// whether it is maximised; none until it is known.
    pub target: Option<(Rect, bool)>,
    /// The view it ends in, whole: nothing more changes.
    pub done: bool,
}

/// How far `at` is into the stretch that starts at `from` and lasts
/// `len`, 0 to 1.
fn part(at: Duration, from: Duration, len: Duration) -> f32 {
    if len.is_zero() {
        return if at >= from { 1.0 } else { 0.0 };
    }
    (at.saturating_sub(from).as_secs_f32() / len.as_secs_f32()).clamp(0.0, 1.0)
}

impl Run {
    /// What only the panel shows, the turn, and what only the window
    /// shows, `at` into the flip.
    fn at(&self, at: Duration) -> (f32, f32, f32) {
        let fade = self.len.mul_f32(PANEL_FADE);
        let turn = self.len.mul_f32(TURN);
        let reveal = self.len.saturating_sub(fade + turn);
        match self.direction {
            Direction::Open => {
                let panel = 1.0 - part(at, Duration::ZERO, fade);
                // Nothing travels before the window is there to travel to.
                let Some(target) = self.target else {
                    return (panel, 0.0, 0.0);
                };
                let from = fade.max(target.ready);
                (
                    panel,
                    ease(part(at, from, turn)),
                    part(at, from + turn, reveal),
                )
            }
            Direction::Close | Direction::Minimize => (
                part(at, reveal + turn, fade),
                1.0 - ease(part(at, reveal, turn)),
                1.0 - part(at, Duration::ZERO, reveal),
            ),
        }
    }

    fn pose(&self, origin: (i32, i32)) -> Pose {
        let at = match self.clock {
            Some(start) if !self.held => start.elapsed(),
            _ => Duration::ZERO,
        };
        let (panel, turn, window) = self.at(at);
        Pose {
            panel,
            turn,
            window,
            target: self.target.map(|t| {
                let r = t.rect;
                (
                    Rect::new(
                        r.left - origin.0,
                        r.top - origin.1,
                        r.right - origin.0,
                        r.bottom - origin.1,
                    ),
                    t.maximized,
                )
            }),
            done: match self.direction {
                Direction::Open => window >= 1.0,
                Direction::Close | Direction::Minimize => panel >= 1.0 && !self.held,
            },
        }
    }
}

/// The flip the panel's window is to draw now, if one runs.
pub fn pose(cx: &App) -> Option<Pose> {
    let run = cx.try_global::<Slot>()?.run.as_ref()?;
    Some(run.pose(panel::origin(cx)?))
}

/// The panel's window has painted the flip at `pose`.
pub fn drew(pose: &Pose, window: &mut Window, cx: &mut App) {
    let Some(run) = cx.default_global::<Slot>().run.as_mut() else {
        return;
    };
    run.frames += 1;
    set_drawn(Some(pose));
    note(format!(
        "frame {} {:.3}/{:.3}/{:.3} lay {:.1} paint {:.1}{}{}",
        run.frames,
        pose.panel,
        pose.turn,
        pose.window,
        TOOK.get().0,
        TOOK.get().1,
        if run.held { " held" } else { "" },
        if pose.target.is_none() {
            " no target"
        } else {
            ""
        },
    ));
    // The clock starts with the first frame on the screen: that frame is
    // the view the flip starts from.
    if run.clock.is_none() && !run.held {
        run.clock = Some(Instant::now());
    }
    crate::frames::frame(&format!(
        "flip-p{:.2}-t{:.3}-w{:.2}",
        pose.panel, pose.turn, pose.window
    ));
    if !pose.done && run.since.elapsed() < STUCK {
        window.request_animation_frame();
        return;
    }
    match run.landed {
        // Once more, the same: by then this frame is on the screen.
        None => {
            run.landed = Some(run.frames);
            window.request_animation_frame();
        }
        Some(_) => {
            crate::timeline::turned(window);
            crate::scene::paint::forget_faded(window);
            window.refresh();
            cx.defer(finish);
        }
    }
}

/// The flip's last frame is on the screen: the view it ends in takes
/// over.
fn finish(cx: &mut App) {
    let Some(run) = cx.default_global::<Slot>().run.take() else {
        return;
    };
    note(format!("finish {:?}", run.direction));
    match run.direction {
        Direction::Open => {
            // The big window, drawn the same under its cloak, shows as
            // the panel's window goes: one step, between two frames.
            if let (Some(big), Some(stage)) = (view::window_hwnd(cx), panel::hwnd(cx)) {
                win::dwm_flush();
                win::uncloak(big);
                win::cloak(stage);
                win::to_front(big);
            }
            panel::close(cx);
        }
        Direction::Close => {
            // It goes anyway.
            cx.default_global::<Slot>()
                .pending
                .retain(|p| !matches!(p, Pending::PanelLeaves));
            panel::slide_away_now(cx);
        }
        Direction::Minimize => {}
    }
    set_drawn(None);
    crate::frames::finish(cx);
    run_pending(cx);
}

/// Whether a flip is drawing now, or about to.
pub fn running(cx: &App) -> bool {
    cx.try_global::<Slot>().is_some_and(|s| {
        s.waiting.is_some_and(|(t, _)| t.elapsed() < STUCK)
            || s.run.as_ref().is_some_and(|r| r.since.elapsed() < STUCK)
    })
}

/// Which way the flip under way, or about to start, goes.
fn direction(cx: &App) -> Option<Direction> {
    let slot = cx.try_global::<Slot>()?;
    slot.run
        .as_ref()
        .map(|r| r.direction)
        .or(slot.waiting.map(|(_, d)| d))
}

/// Keeps `what` for when the flip under way has ended.
fn queue(what: Pending, cx: &mut App) {
    cx.default_global::<Slot>().pending.push(what);
}

/// The flip has ended (or never started): does what was asked meanwhile.
fn run_pending(cx: &mut App) {
    let pending = std::mem::take(&mut cx.default_global::<Slot>().pending);
    for what in pending {
        // One may start another flip: the rest wait for that one.
        if running(cx) {
            queue(what, cx);
            continue;
        }
        match what {
            Pending::ShowWindow(anchor) => show_window(anchor, cx),
            Pending::FromPanel(focus) => {
                if let Some(handle) = cx.try_global::<panel::PanelSlot>().and_then(|s| s.handle()) {
                    handle
                        .update(cx, |_, window, cx| open_window(window, focus, cx))
                        .ok();
                }
            }
            Pending::TogglePanel(anchor) => panel::toggle(anchor, cx),
            Pending::PanelLeaves => panel::slide_away(cx),
            Pending::WindowCloses => {
                if let Some(handle) = view::window_handle(cx) {
                    handle
                        .update(cx, |_, window, cx| {
                            if !flip_back(window, cx) {
                                window.remove_window();
                            }
                        })
                        .ok();
                }
            }
        }
    }
}

/// The panel is asked to toggle (the tray, the badge): true when a flip
/// under way takes that for after it ends.
pub fn hold_toggle(anchor: panel::Anchor, cx: &mut App) -> bool {
    if !running(cx) {
        return false;
    }
    queue(Pending::TogglePanel(anchor), cx);
    true
}

/// The panel is to go (Esc, a click elsewhere, focus lost): true when a
/// flip under way owns it, and lets it go once that ends. A panel the flip
/// starts from goes with the flip anyway; one it ends in goes after.
pub fn hold_leave(cx: &mut App) -> bool {
    if !running(cx) {
        return false;
    }
    let slot = cx.default_global::<Slot>();
    // Waiting to start from the panel: the flip is called off, and the
    // panel goes as asked.
    if slot.run.is_none() && matches!(slot.waiting, Some((_, Direction::Open))) {
        slot.called_off = true;
        slot.waiting = None;
        return false;
    }
    let ends_in_panel = matches!(
        slot.run.as_ref().map(|r| r.direction),
        Some(Direction::Close | Direction::Minimize)
    ) || matches!(
        slot.waiting,
        Some((_, Direction::Close | Direction::Minimize))
    );
    if ends_in_panel
        && !slot
            .pending
            .iter()
            .any(|p| matches!(p, Pending::PanelLeaves))
    {
        slot.pending.push(Pending::PanelLeaves);
    }
    true
}

/// How long a flip takes, as config says.
fn length(cx: &App) -> Duration {
    let ms = cx
        .try_global::<Ui>()
        .map_or(DEFAULT_MS as u32, |ui| ui.state.read(cx).config.flip_ms);
    Duration::from_millis(u64::from(ms.clamp(100, 10_000)))
}

/// Glides every tree back to its newest commits, where the flip starts
/// from: true when they were elsewhere, after which `then` runs once
/// they're there and drawn there.
fn newest_first(direction: Direction, then: impl FnOnce(&mut App) + 'static, cx: &mut App) -> bool {
    let Some(t) = cx.try_global::<Ui>().map(|ui| ui.timelines.clone()) else {
        return false;
    };
    let from_panel = direction == Direction::Open;
    // A scroll you can follow, paced with the flip: the trees to their
    // newest commits, and the view the flip starts from to its start, the
    // panel's list to its top or the window's table to its left edge (and
    // its pull requests to their top).
    let wait = SCROLL_HOME.mul_f32(length(cx).as_millis() as f32 / DEFAULT_MS);
    let trees = t.update(cx, |t, cx| t.glide_newest(wait, cx));
    let view = view::glide_to_start(from_panel, wait, cx);
    note(format!(
        "{direction:?}: glide home: trees {trees}, view {view}"
    ));
    if !trees && !view {
        return false;
    }
    begin_wait(direction, cx);
    let start = Instant::now();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(wait).await;
        // Until the glides have ended and the views have drawn their
        // last step: asked each frame or so, never guessed.
        loop {
            let done = cx
                .update(|cx| {
                    let gliding = view::gliding()
                        || cx
                            .try_global::<Ui>()
                            .is_some_and(|ui| ui.timelines.read(cx).gliding());
                    let drawn = view::last_glide_end()
                        .is_none_or(|end| PAINTED.get().is_some_and(|t| t > end));
                    (!gliding && drawn) || start.elapsed() > wait + DRAW_WAIT
                })
                .unwrap_or(true);
            if done {
                break;
            }
            cx.background_executor().timer(POLL).await;
        }
        cx.update(|cx| {
            note(format!(
                "glided home after {} ms",
                start.elapsed().as_millis()
            ));
            let slot = cx.default_global::<Slot>();
            slot.waiting = None;
            if std::mem::take(&mut slot.called_off) {
                run_pending(cx);
                return;
            }
            then(cx);
            if !running(cx) {
                run_pending(cx);
            }
        })
        .ok();
    })
    .detach();
    true
}

/// Marks the flip as about to start, so nothing else moves its windows.
fn begin_wait(direction: Direction, cx: &mut App) {
    let slot = cx.default_global::<Slot>();
    slot.waiting = Some((Instant::now(), direction));
    slot.called_off = false;
}

fn enabled(cx: &App) -> bool {
    cx.try_global::<Ui>().is_some_and(|ui| {
        let cfg = &ui.state.read(cx).config;
        cfg.flip_animation && cfg.window && cfg.panel
    })
}

/// A filter typed in one view would be cleared by the other as it opens:
/// cleared before the flip instead, so the flip turns what that one shows.
fn clear_filter(cx: &mut App) {
    if let Some(state) = cx.try_global::<Ui>().map(|ui| ui.state.clone())
        && !state.read(cx).filter.is_empty()
    {
        state.update(cx, |s, cx| {
            s.filter.clear();
            cx.notify();
        });
    }
}

/// Whether the flip can draw a window whose inside is `rect` on the
/// screen at scale `scale`: wholly inside the panel's window, at its
/// scale.
fn fits_stage(rect: Rect, scale: Option<f32>, cx: &App) -> bool {
    let Some(area) = panel::area(cx) else {
        return false;
    };
    let stage = panel::hwnd(cx).and_then(win::window_scale);
    let same = match (scale, stage) {
        (Some(a), Some(b)) => (a - b).abs() < 0.01,
        _ => false,
    };
    same && rect.left >= area.left
        && rect.top >= area.top
        && rect.right <= area.right
        && rect.bottom <= area.bottom
        && rect.width() > 0
        && rect.height() > 0
}

/// Redraws the panel's window: a flip's next frame.
fn redraw_stage(cx: &mut App) {
    if let Some(handle) = cx.try_global::<panel::PanelSlot>().and_then(|s| s.handle()) {
        handle.update(cx, |_, window, _| window.refresh()).ok();
    }
}

/// Opens the big window from the panel, scrolled to `focus` if given, with
/// the flip when it's on. Called from the panel's own handlers; closes the
/// panel either way.
pub fn open_window(panel: &mut Window, focus: Option<PathBuf>, cx: &mut App) {
    if running(cx) {
        // The panel a flip back is ending in: once that flip has ended.
        if matches!(direction(cx), Some(Direction::Close | Direction::Minimize)) {
            queue(Pending::FromPanel(focus), cx);
        }
        return;
    }
    // A panel still sliding in finishes first, so the flip starts from it
    // where it rests; one sliding away is going, and takes the click with it.
    match panel::slide_state(cx) {
        panel::SlideState::Leaving => return,
        panel::SlideState::Arriving(left) => {
            let handle = panel.window_handle();
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(left + Duration::from_millis(20))
                    .await;
                handle
                    .update(cx, |_, window, cx| open_window(window, focus, cx))
                    .ok();
            })
            .detach();
            return;
        }
        panel::SlideState::Still => {}
    }
    if let Some(path) = &focus
        && let Some(t) = cx.try_global::<Ui>().map(|ui| ui.timelines.clone())
    {
        t.update(cx, |t, _| t.focus(path));
    }
    let flip = enabled(cx) && !view::window_open(cx) && panel::shows(cx);
    if flip {
        clear_filter(cx);
        let handle = panel.window_handle();
        let again = focus.clone();
        if newest_first(
            Direction::Open,
            move |cx| {
                // Dismissed meanwhile: it goes, and nothing opens.
                if !panel::is_open(cx) || panel::is_leaving(cx) {
                    return;
                }
                handle
                    .update(cx, |_, window, cx| open_window(window, again, cx))
                    .ok();
            },
            cx,
        ) {
            panel.refresh();
            return;
        }
    }
    if flip {
        start_open(cx);
        // Its hovered row and buttons lose their hover as it starts.
        panel.refresh();
    } else {
        panel::dismiss(panel, cx);
        view::show_window(cx);
    }
}

/// Starts the flip into the big window: what only the panel shows starts
/// to fade at once, while the big window comes up, cloaked, to be
/// travelled to as soon as it is where it belongs and has drawn itself.
fn start_open(cx: &mut App) {
    note("start open");
    cx.default_global::<Slot>().run = Some(Run {
        direction: Direction::Open,
        len: length(cx),
        clock: None,
        held: false,
        target: None,
        frames: 0,
        landed: None,
        since: Instant::now(),
    });
    crate::frames::start(win::primary_monitor().rect, cx);
    if let Some(t) = cx.try_global::<Ui>().map(|ui| ui.timelines.clone()) {
        t.update(cx, |t, cx| t.drop_hover(cx));
    }
    crate::overlay::over_flip(cx);
    let maximized = view::opens_maximized(cx);
    let before = Instant::now();
    view::show_window_cloaked(cx);
    note(format!(
        "big window made in {} ms",
        before.elapsed().as_millis()
    ));
    let opened = Instant::now();
    cx.spawn(async move |cx| {
        // Where its inside came out, once that has stopped changing (a
        // maximised window shows at its normal size first), and it has
        // drawn itself there.
        let mut seen: Option<(Rect, u32)> = None;
        let mut drawn_from: Option<u64> = None;
        let found = loop {
            cx.background_executor().timer(POLL).await;
            let step = cx.update(|cx| {
                if cx.default_global::<Slot>().run.is_none() {
                    return Some(None);
                }
                if opened.elapsed() > OPEN_WAIT {
                    return Some(None);
                }
                let h = view::window_hwnd(cx)?;
                let rect = win::client_rect(h).filter(|_| win::is_zoomed(h) == maximized)?;
                match &mut seen {
                    Some((last, n)) if *last == rect => *n += 1,
                    _ => {
                        seen = Some((rect, 0));
                        drawn_from = None;
                    }
                }
                if seen.is_some_and(|(_, n)| n < 2) {
                    return None;
                }
                match drawn_from {
                    None => {
                        drawn_from = Some(WINDOW_FRAMES.get());
                        if let Some(handle) = view::window_handle(cx) {
                            handle.update(cx, |_, window, _| window.refresh()).ok();
                        }
                        None
                    }
                    Some(from) if WINDOW_FRAMES.get() > from => {
                        Some(Some((rect, win::window_scale(h))))
                    }
                    Some(_) => None,
                }
            });
            match step {
                Ok(Some(found)) => break found,
                Ok(None) => {}
                Err(_) => break None,
            }
        };
        cx.update(|cx| {
            let fits = found.is_some_and(|(rect, scale)| fits_stage(rect, scale, cx));
            let slot = cx.default_global::<Slot>();
            let Some(run) = slot.run.as_mut() else {
                return;
            };
            note(format!(
                "window {found:?} fits {fits} after {} ms",
                opened.elapsed().as_millis()
            ));
            match found.filter(|_| fits) {
                Some((rect, _)) => {
                    run.target = Some(Target {
                        rect,
                        maximized,
                        ready: run.clock.map_or(Duration::ZERO, |c| c.elapsed()),
                    });
                    redraw_stage(cx);
                }
                // It can't be drawn in the panel's window (another
                // screen, another scale), or never came: it just shows.
                None => {
                    slot.run = None;
                    if let Some(h) = view::window_hwnd(cx) {
                        win::uncloak(h);
                        win::to_front(h);
                    }
                    panel::close(cx);
                    crate::frames::finish(cx);
                    run_pending(cx);
                }
            }
        })
        .ok();
    })
    .detach();
}

/// Opens the big window from the tray or the badge: through the panel at
/// `anchor` when the flip is on, else straight away. During a flip, once
/// it has ended.
pub fn show_window(anchor: panel::Anchor, cx: &mut App) {
    if running(cx) {
        queue(Pending::ShowWindow(anchor), cx);
        return;
    }
    if !enabled(cx) || view::window_open(cx) {
        if !view::window_open(cx) {
            panel::close(cx);
        }
        view::show_window(cx);
        return;
    }
    panel::then_window(anchor, cx);
}

/// Closing the big window: starts the flip back into the panel (which then
/// slides away, or stays when the window was minimised) and returns
/// true, leaving the window for the flip to close; false when the flip is
/// off or can't run, so the caller closes it.
pub fn flip_back(big: &mut Window, cx: &mut App) -> bool {
    let minimized = win::take_minimized();
    if running(cx) {
        // Opening into it: closed once it has opened. Closing already:
        // the flip closes it.
        if direction(cx) == Some(Direction::Open)
            && !cx
                .default_global::<Slot>()
                .pending
                .iter()
                .any(|p| matches!(p, Pending::WindowCloses))
        {
            queue(Pending::WindowCloses, cx);
        }
        return true;
    }
    // Opens there again next time.
    view::remember_place(big, cx);
    if !enabled(cx) || panel::guess_place(cx).is_none() {
        return false;
    }
    clear_filter(cx);
    let direction = if minimized {
        Direction::Minimize
    } else {
        Direction::Close
    };
    // The columns back at their newest commits first; then the flip, or,
    // should it not start, the window just closes. Outside this handler
    // of the window's own, where the window can't be asked anything.
    let start = move |cx: &mut App| {
        cx.default_global::<Slot>().waiting = None;
        if !start_close(direction, cx) {
            panel::close(cx);
            view::close_window(cx);
        }
    };
    if !newest_first(direction, start, cx) {
        begin_wait(direction, cx);
        cx.defer(move |cx| {
            start(cx);
            if !running(cx) {
                run_pending(cx);
            }
        });
    }
    true
}

/// Starts the flip back into the panel: the panel's window comes up
/// cloaked, drawing the big window where it is; once it has, it takes
/// the big window's place on the screen in one step, and only then does
/// anything move. False when it can't.
fn start_close(direction: Direction, cx: &mut App) -> bool {
    let Some(big) = view::window_hwnd(cx) else {
        return false;
    };
    let (Some(rect), maximized) = (win::client_rect(big), win::is_zoomed(big)) else {
        return false;
    };
    note(format!("start {direction:?} from {rect:?}"));
    cx.default_global::<Slot>().run = Some(Run {
        direction,
        len: length(cx),
        clock: None,
        held: true,
        target: Some(Target {
            rect,
            maximized,
            ready: Duration::ZERO,
        }),
        frames: 0,
        landed: None,
        since: Instant::now(),
    });
    let call_off = |cx: &mut App| {
        cx.default_global::<Slot>().run = None;
        false
    };
    let before = Instant::now();
    if !panel::reopen_cloaked(cx) {
        return call_off(cx);
    }
    note(format!(
        "panel's window made in {} ms",
        before.elapsed().as_millis()
    ));
    crate::overlay::over_flip(cx);
    if !fits_stage(rect, win::window_scale(big), cx) {
        return call_off(cx);
    }
    crate::frames::start(win::primary_monitor().rect, cx);
    if let Some(t) = cx.try_global::<Ui>().map(|ui| ui.timelines.clone()) {
        t.update(cx, |t, cx| t.drop_hover(cx));
    }
    let opened = Instant::now();
    cx.spawn(async move |cx| {
        // Until the panel's window is where it belongs and has drawn the
        // big window there, twice: the second is surely of that place.
        let mut from: Option<u32> = None;
        let ready = loop {
            cx.background_executor().timer(POLL).await;
            let step = cx.update(|cx| {
                let frames = cx.default_global::<Slot>().run.as_ref().map(|r| r.frames);
                let Some(frames) = frames else {
                    return Some(false);
                };
                if opened.elapsed() > DRAW_WAIT {
                    return Some(false);
                }
                if !panel::placed(cx) {
                    from = None;
                    return None;
                }
                match from {
                    None => {
                        from = Some(frames);
                        redraw_stage(cx);
                        None
                    }
                    Some(from) if frames >= from + 2 => Some(true),
                    Some(_) => {
                        redraw_stage(cx);
                        None
                    }
                }
            });
            match step {
                Ok(Some(ready)) => break ready,
                Ok(None) => {}
                Err(_) => break false,
            }
        };
        cx.update(|cx| {
            let stage = panel::hwnd(cx);
            let slot = cx.default_global::<Slot>();
            let Some(run) = slot.run.as_mut() else {
                return;
            };
            note(format!(
                "stage ready {ready} after {} ms, {} frames",
                opened.elapsed().as_millis(),
                run.frames
            ));
            match stage.filter(|_| ready) {
                Some(stage) => {
                    run.held = false;
                    // One step, between two frames: the panel's window,
                    // showing the big window, in the big window's place.
                    let at = Instant::now();
                    win::dwm_flush();
                    let flushed = at.elapsed();
                    win::uncloak(stage);
                    win::cloak(big);
                    let swapped = at.elapsed();
                    // The big window is off the screen: the panel takes
                    // the keyboard.
                    panel::activate(cx);
                    // Activating put it over the badge: the badge back
                    // over it at once.
                    crate::overlay::over_flip(cx);
                    let front = at.elapsed();
                    view::close_window(cx);
                    redraw_stage(cx);
                    note(format!(
                        "swap: flushed {flushed:?}, swapped {swapped:?}, in front {front:?}, big window closed {:?}",
                        at.elapsed()
                    ));
                }
                None => {
                    slot.run = None;
                    panel::close(cx);
                    view::close_window(cx);
                    crate::frames::finish(cx);
                    run_pending(cx);
                }
            }
        })
        .ok();
    })
    .detach();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(direction: Direction, target: bool) -> Run {
        Run {
            direction,
            len: Duration::from_millis(1000),
            clock: None,
            held: false,
            target: target.then_some(Target {
                rect: Rect::new(0, 0, 100, 100),
                maximized: false,
                ready: Duration::ZERO,
            }),
            frames: 0,
            landed: None,
            since: Instant::now(),
        }
    }

    #[test]
    fn opening_starts_as_the_panel_and_ends_as_the_window() {
        let r = run(Direction::Open, true);
        assert_eq!(r.at(Duration::ZERO), (1.0, 0.0, 0.0));
        assert_eq!(r.at(Duration::from_millis(1000)), (0.0, 1.0, 1.0));
    }

    #[test]
    fn closing_starts_as_the_window_and_ends_as_the_panel() {
        for d in [Direction::Close, Direction::Minimize] {
            let r = run(d, true);
            assert_eq!(r.at(Duration::ZERO), (0.0, 1.0, 1.0));
            assert_eq!(r.at(Duration::from_millis(1000)), (1.0, 0.0, 0.0));
        }
    }

    #[test]
    fn nothing_fades_while_anything_moves() {
        for d in [Direction::Open, Direction::Close] {
            let r = run(d, true);
            for ms in 0..=1000 {
                let (panel, turn, window) = r.at(Duration::from_millis(ms));
                let moving = turn > 0.0 && turn < 1.0;
                let fading = (panel > 0.0 && panel < 1.0) || (window > 0.0 && window < 1.0);
                assert!(!(moving && fading), "{d:?} at {ms} ms");
                // And only the view nearer shows its own.
                assert!(panel == 0.0 || window == 0.0, "{d:?} at {ms} ms");
            }
        }
    }

    #[test]
    fn opening_waits_for_the_window_before_it_travels() {
        let mut r = run(Direction::Open, false);
        assert_eq!(r.at(Duration::from_millis(600)), (0.0, 0.0, 0.0));
        r.target = Some(Target {
            rect: Rect::new(0, 0, 100, 100),
            maximized: false,
            ready: Duration::from_millis(600),
        });
        assert_eq!(r.at(Duration::from_millis(600)), (0.0, 0.0, 0.0));
        let (_, turn, _) = r.at(Duration::from_millis(900));
        assert!(turn > 0.0 && turn < 1.0);
        assert_eq!(r.at(Duration::from_millis(1600)), (0.0, 1.0, 1.0));
    }
}
