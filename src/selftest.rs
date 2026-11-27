//! gitstat testing itself on a real screen: `gitstat --self-test DIR`
//! opens its views by itself, records what shows, writes what it saw to
//! `DIR/report.txt` and quits. Run by the quick tests (`tests/self_test.rs`)
//! beside whatever gitstat is running, with a config and a data folder of
//! its own, so nothing of the user's is touched.
//!
//! The pictures go where the config's frame recorder puts them: every
//! frame of a view coming or going, and stills of it at rest.

use crate::layout::Rect;
use crate::panel::{self, Anchor};
use crate::view::{self, Ui};
use crate::win;
use gpui::{App, AppContext, AsyncApp};
use image::ImageEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Runs the test, then quits the app.
pub fn start(out: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx| {
        let mut report = String::new();
        // The mouse, off the views while they are compared px for px (a
        // row under it lights up), and back after.
        let mouse = win::cursor_pos();
        let work = win::primary_monitor().work;
        win::set_cursor_pos(work.left + 150, work.top + 60);
        let verdict = run(&out, &mut report, cx).await;
        win::set_cursor_pos(mouse.0, mouse.1);
        let _ = writeln!(
            report,
            "{}",
            match &verdict {
                Ok(()) => "result: done".to_string(),
                Err(why) => format!("result: failed: {why}"),
            }
        );
        let _ = std::fs::write(out.join("report.txt"), report);
        // The recorder's last pictures are still being written.
        cx.background_executor()
            .timer(Duration::from_millis(4000))
            .await;
        std::process::exit(i32::from(verdict.is_err()));
    })
    .detach();
}

async fn pause(ms: u64, cx: &mut AsyncApp) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

/// A picture of part of the screen: its place, and its pixels as RGBA.
struct Shot {
    rect: Rect,
    w: u32,
    pixels: Vec<u8>,
}

impl Shot {
    /// The colour at screen px (`x`, `y`), which must be inside it.
    fn at(&self, x: i32, y: i32) -> [u8; 3] {
        let i =
            ((y - self.rect.top) as usize * self.w as usize + (x - self.rect.left) as usize) * 4;
        match self.pixels.get(i..i + 3) {
            Some(p) => [p[0], p[1], p[2]],
            None => [0; 3],
        }
    }

    /// How bright it is over `r` (screen px, cut to what it shows), all
    /// channels summed.
    fn light(&self, r: Rect) -> u64 {
        let h = (self.pixels.len() / 4 / self.w.max(1) as usize) as i32;
        let r = Rect::new(
            r.left.max(self.rect.left),
            r.top.max(self.rect.top),
            r.right.min(self.rect.right),
            r.bottom.min(self.rect.top + h),
        );
        let mut sum = 0u64;
        for y in r.top..r.bottom {
            for x in r.left..r.right {
                sum += self.at(x, y).iter().map(|&c| u64::from(c)).sum::<u64>();
            }
        }
        sum
    }

    /// How far apart `self` and `other` are at most, in any channel, over
    /// `r` (screen px, cut to what both show).
    fn differs(&self, other: &Shot, r: Rect) -> u8 {
        let r = Rect::new(
            r.left.max(self.rect.left).max(other.rect.left),
            r.top.max(self.rect.top).max(other.rect.top),
            r.right.min(self.rect.right).min(other.rect.right),
            r.bottom.min(self.rect.bottom).min(other.rect.bottom),
        );
        let mut most = 0;
        for y in r.top..r.bottom {
            for x in r.left..r.right {
                let (a, b) = (self.at(x, y), other.at(x, y));
                for c in 0..3 {
                    most = most.max(a[c].abs_diff(b[c]));
                }
            }
        }
        most
    }
}

/// The screen inside `rect` now, also saved as `name`.png in `out`: taken
/// and written off this thread, and done when this returns.
async fn shot(out: &std::path::Path, name: &str, rect: Rect, cx: &mut AsyncApp) -> Option<Shot> {
    let file = out.join(format!("{name}.png"));
    cx.background_spawn(async move {
        let (w, h, pixels) = win::capture(rect)?;
        let _ = image::save_buffer(file, &pixels, w, h, image::ExtendedColorType::Rgba8);
        Some(Shot { rect, w, pixels })
    })
    .await
}

async fn run(out: &std::path::Path, report: &mut String, cx: &mut AsyncApp) -> Result<(), String> {
    let gone = |_| "the app went away".to_string();
    // The repos, and a moment for their trees.
    let began = Instant::now();
    loop {
        let ready = cx
            .update(|cx| {
                cx.try_global::<Ui>()
                    .is_some_and(|ui| ui.state.read(cx).has_result)
            })
            .map_err(gone)?;
        if ready {
            break;
        }
        if began.elapsed() > Duration::from_secs(40) {
            return Err("no repos found in 40 s".into());
        }
        pause(100, cx).await;
    }
    pause(2500, cx).await;
    // GitHub's pull requests come later: the panel is tested with them.
    let waited = Instant::now();
    let prs = loop {
        let (prs, problem) = cx
            .update(|cx| {
                let s = cx.global::<Ui>().state.read(cx);
                (s.open_prs().len(), s.github_problem.clone())
            })
            .map_err(gone)?;
        if prs > 0 || problem.is_some() || waited.elapsed() > Duration::from_secs(12) {
            break (prs, problem);
        }
        pause(200, cx).await;
    };
    let _ = writeln!(
        report,
        "pull requests: {} open, problem {:?}, after {} ms more",
        prs.0,
        prs.1,
        waited.elapsed().as_millis()
    );
    pause(500, cx).await;
    let mon = win::primary_monitor();
    let _ = writeln!(
        report,
        "monitor: {:?} work {:?} scale {}",
        mon.rect, mon.work, mon.scale
    );
    let repos = cx
        .update(|cx| cx.global::<Ui>().state.read(cx).statuses.len())
        .map_err(gone)?;
    let _ = writeln!(
        report,
        "repos: {repos}, found in {} ms",
        began.elapsed().as_millis()
    );

    // The panel: slides in by the tray, rests, slides away.
    let anchor = Anchor::Tray(win::tray_area());
    let around = |r: Rect| {
        let room = (220.0 * mon.scale) as i32;
        let r = r.outset(room);
        Rect::new(
            r.left.max(mon.rect.left),
            r.top.max(mon.rect.top),
            r.right.min(mon.rect.right),
            r.bottom.min(mon.rect.bottom),
        )
    };
    let place = cx.update(panel::guess_place).map_err(gone)?;
    let Some(place) = place else {
        return Err("no place for the panel".into());
    };
    // The whole screen: with the badge showing, the guess is beside the
    // badge, not by the tray where this opens it.
    let before = shot(out, "panel-before", mon.rect, cx).await;
    let opened = Instant::now();
    cx.update(|cx| {
        crate::frames::crop(Some(around(place)));
        panel::toggle(anchor, cx);
    })
    .map_err(gone)?;
    let mut shown_after = None;
    while opened.elapsed() < Duration::from_millis(1500) {
        pause(10, cx).await;
        let shown = cx.update(|cx| panel::shows(cx)).map_err(gone)?;
        if shown && shown_after.is_none() {
            shown_after = Some(opened.elapsed());
        }
    }
    let facts = cx.update(|cx| panel::facts(cx)).map_err(gone)?;
    let _ = writeln!(report, "panel: seen shown after {shown_after:?}\n{facts}");
    let rect = cx.update(|cx| panel::last_rect(cx)).map_err(gone)?;
    let Some(rect) = rect else {
        return Err("the panel has no place".into());
    };
    if shown_after.is_none() {
        return Err("the panel never showed".into());
    }
    // Clicks beside the panel must reach what is under its window.
    if let Some((h, x, y)) = cx.update(|cx| panel::click_test(cx)).map_err(gone)? {
        let passes = unsafe { win::passes_clicks(h, x, y) };
        let _ = writeln!(
            report,
            "panel: a click beside it at ({x}, {y}) passes through: {passes:?}"
        );
        if passes != Some(true) {
            return Err("clicks beside the panel do not pass through its window".into());
        }
    }
    let rest = shot(out, "panel-rest", around(rect), cx).await;
    shot(
        out,
        "panel-only",
        rect.outset((12.0 * mon.scale) as i32),
        cx,
    )
    .await;
    if let (Some(before), Some(rest)) = (&before, &rest) {
        // What the panel changed on the screen around itself: its rim just
        // outside it, its shadow fading out from there, and nothing far
        // from it.
        let (mx, my) = rect.centre();
        let _ = writeln!(
            report,
            "panel: sheet {:?} at its middle; rim left {:?} top {:?} right {:?} bottom {:?}",
            rest.at(mx, my),
            rest.at(rect.left - 1, my),
            rest.at(mx, rect.top - 1),
            rest.at(rect.right, my),
            rest.at(mx, rect.bottom),
        );
        let mut shadow = String::new();
        for d in [2, 4, 8, 12, 16, 24, 32, 48, 64, 96] {
            let below = Rect::new(mx - 40, rect.bottom + d, mx + 40, rect.bottom + d + 1);
            let right = Rect::new(rect.right + d, my - 40, rect.right + d + 1, my + 40);
            let above = Rect::new(mx - 40, rect.top - d - 1, mx + 40, rect.top - d);
            let _ = write!(
                shadow,
                " {d}px: {}/{}/{}",
                before.differs(rest, below),
                before.differs(rest, right),
                before.differs(rest, above),
            );
        }
        let _ = writeln!(
            report,
            "panel: change from the screen before, below/right/above its edge:{shadow}"
        );
        // The shadow alone: how much light is left of what was there, in
        // a line along each side at each distance (100 = untouched).
        let mut left_of = String::new();
        for d in [1, 2, 4, 8, 12, 16, 24, 32, 48, 64, 96] {
            let rim = 2;
            let bands = [
                Rect::new(
                    mx - 60,
                    rect.bottom + rim + d,
                    mx + 60,
                    rect.bottom + rim + d + 1,
                ),
                Rect::new(
                    rect.right + rim + d,
                    my - 60,
                    rect.right + rim + d + 1,
                    my + 60,
                ),
                Rect::new(mx - 60, rect.top - rim - d - 1, mx + 60, rect.top - rim - d),
                Rect::new(
                    rect.left - rim - d - 1,
                    my - 60,
                    rect.left - rim - d,
                    my + 60,
                ),
            ];
            let _ = write!(left_of, " {d}px:");
            for band in bands {
                // Over a dark screen a shadow takes nothing to measure.
                let was = before.light(band);
                let was = if was < 120 * 3 * 40 { 0 } else { was };
                match (rest.light(band) * 100).checked_div(was) {
                    Some(left) => {
                        let _ = write!(left_of, " {left}");
                    }
                    None => {
                        let _ = write!(left_of, " -");
                    }
                }
            }
        }
        let _ = writeln!(
            report,
            "panel: light left under its shadow, % below/right/above/left:{left_of}"
        );
        let far = (140.0 * mon.scale) as i32;
        let strip = Rect::new(
            rect.right + far,
            rect.top,
            rect.right + far + 60,
            rect.bottom,
        );
        let _ = writeln!(
            report,
            "panel: change far to its right (must be 0): {}",
            before.differs(rest, strip)
        );
    }
    cx.update(|cx| {
        crate::frames::crop(Some(around(rect)));
        panel::slide_away(cx);
    })
    .map_err(gone)?;
    pause(1200, cx).await;
    let open = cx.update(|cx| panel::is_open(cx)).map_err(gone)?;
    let _ = writeln!(report, "panel: open after sliding away: {open}");
    let after = shot(out, "panel-gone", around(rect), cx).await;
    if let (Some(before), Some(after)) = (&before, &after) {
        let _ = writeln!(
            report,
            "panel: left behind on the screen (must be 0): {}",
            before.differs(after, around(rect))
        );
    }
    if open {
        return Err("the panel did not close".into());
    }

    // The big window: opens, rests, closes.
    cx.update(|cx| {
        crate::frames::crop(None);
        view::show_window(cx);
    })
    .map_err(gone)?;
    pause(2500, cx).await;
    let client = cx
        .update(|cx| view::window_hwnd(cx).and_then(win::client_rect))
        .map_err(gone)?;
    let hwnd = cx.update(view::window_hwnd).map_err(gone)?;
    let frame = hwnd.and_then(win::window_rect);
    let _ = writeln!(report, "window: frame {frame:?} client {client:?}");
    let whole = shot(out, "window-rest", mon.rect, cx).await;
    let (Some(h), Some(client)) = (hwnd, client) else {
        return Err("the big window did not open".into());
    };
    let inside = client.left >= mon.work.left
        && client.top >= mon.work.top
        && client.right <= mon.work.right
        && client.bottom <= mon.work.bottom;
    let _ = writeln!(report, "window: inside the work area: {inside}");
    // Its own title bar: its colour, and Windows finding its parts where
    // they are drawn.
    let e = |v: f32| (v * mon.scale).round() as i32;
    let mid = client.top + e(16.0);
    let button = |i: i32| client.right - e(46.0) * (2 - i) - e(23.0);
    if let Some(whole) = &whole {
        let _ = writeln!(
            report,
            "window: title bar {:?} beside its name, {:?} between name and buttons; top bar {:?}; \
             rim left {:?} top {:?}",
            whole.at(client.left + e(4.0), mid),
            whole.at((client.left + button(0)) / 2, mid),
            whole.at(client.left + e(4.0), client.top + e(36.0)),
            whole.at(client.left - 1, mid),
            whole.at(client.left + e(100.0), client.top - 1),
        );
    }
    let hits = unsafe {
        [
            win::hit_test(h, (client.left + button(0)) / 2, mid),
            win::hit_test(h, button(0), mid),
            win::hit_test(h, button(1), mid),
            win::hit_test(h, button(2), mid),
            win::hit_test(h, (client.left + button(0)) / 2, client.top + 1),
            win::hit_test(h, client.left + e(100.0), client.top + e(60.0)),
        ]
    };
    let _ = writeln!(
        report,
        "window: Windows finds title bar, minimise, maximise, close, top edge, inside \
         (want 2, 8, 9, 20, 12, 1): {hits:?}"
    );
    pause(300, cx).await;
    shot(
        out,
        "window-title-bar",
        Rect::new(
            client.left - e(12.0),
            client.top - e(12.0),
            client.right + e(12.0),
            client.top + e(110.0),
        ),
        cx,
    )
    .await;
    shot(
        out,
        "window-bottom",
        Rect::new(
            client.left - e(12.0),
            client.bottom - e(300.0),
            client.left + e(700.0),
            client.bottom + e(12.0),
        ),
        cx,
    )
    .await;
    if !inside {
        return Err("the big window reaches off the work area".into());
    }
    if hits[..4] != [Some(2), Some(8), Some(9), Some(20)] {
        return Err("Windows does not find the title bar's parts where they are drawn".into());
    }
    cx.update(view::close_window).map_err(gone)?;
    pause(500, cx).await;

    // The flip: panel to big window and back (closed, then minimised),
    // every frame that can be caught, against the views at rest.
    let window_rest = whole;
    crate::frames::mute(true);
    cx.update(|cx| panel::toggle(anchor, cx)).map_err(gone)?;
    pause(1500, cx).await;
    let panel_rest = shot(out, "flip-panel-rest", mon.rect, cx).await;
    let (Some(panel_rest), Some(window_rest)) = (panel_rest, window_rest) else {
        return Err("no picture of the screen".into());
    };
    let badge = cx.update(|cx| crate::overlay::placed(cx)).map_err(gone)?;
    let _ = writeln!(
        report,
        "flip: the mouse is at {:?}, the badge at {badge:?}",
        win::cursor_pos()
    );
    let (panel_rest, window_rest) = (Arc::new(panel_rest), Arc::new(window_rest));
    let mut window_now = window_rest.clone();
    for (round, minimise) in [(1, false), (2, true)] {
        let what = format!("flip open {round}");
        if !cx.update(|cx| panel::is_open(cx)).map_err(gone)? {
            cx.update(|cx| panel::toggle(anchor, cx)).map_err(gone)?;
            pause(1500, cx).await;
        }
        gear_log(report, "between flips");
        let watch = Watch::start(mon.rect, out.join(what.replace(' ', "-")));
        pause(120, cx).await;
        let asked = cx
            .update(|cx| {
                let handle = cx
                    .try_global::<panel::PanelSlot>()
                    .and_then(|s| s.handle())?;
                handle
                    .update(cx, |_, window, cx| {
                        crate::flip::open_window(window, None, cx);
                    })
                    .ok()
            })
            .map_err(gone)?;
        if asked.is_none() {
            return Err("no panel to flip from".into());
        }
        flip_ends(cx).await?;
        pause(300, cx).await;
        let frames = watch.stop(cx).await;
        let open = cx.update(|cx| view::window_open(cx)).map_err(gone)?;
        let now = cx
            .update(|cx| view::window_hwnd(cx).and_then(win::client_rect))
            .map_err(gone)?;
        let _ = writeln!(
            report,
            "{what}: window open {open}, client {now:?} (was {client:?}), panel open {}",
            cx.update(|cx| panel::is_open(cx)).map_err(gone)?
        );
        tell(
            report,
            out,
            &what,
            &frames,
            (&panel_rest, rect, (1000, 0, 0)),
            (&window_rest, client, (0, 1000, 1000)),
            badge,
        );
        drop(frames);
        if !open {
            return Err("the flip did not open the big window".into());
        }
        pause(600, cx).await;
        let Some(now) = shot(out, &format!("flip-window-rest-{round}"), mon.rect, cx).await else {
            return Err("no picture of the screen".into());
        };
        let _ = writeln!(
            report,
            "{what}: the window it ended in against the window opened plainly, px that differ \
             inside it: {}",
            changed(&now, &window_rest, client)
        );
        window_now = Arc::new(now);

        let what = format!(
            "flip {} {round}",
            if minimise { "minimise" } else { "close" }
        );
        gear_log(report, "between flips");
        let watch = Watch::start(mon.rect, out.join(what.replace(' ', "-")));
        pause(120, cx).await;
        let asked = cx
            .update(|cx| {
                let handle = view::window_handle(cx)?;
                if minimise {
                    win::mark_minimized();
                }
                handle
                    .update(cx, |_, window, cx| crate::flip::flip_back(window, cx))
                    .ok()
            })
            .map_err(gone)?;
        let _ = writeln!(report, "{what}: taken by the flip: {asked:?}");
        if asked != Some(true) {
            return Err("the flip back did not start".into());
        }
        flip_ends(cx).await?;
        pause(if minimise { 300 } else { 900 }, cx).await;
        let frames = watch.stop(cx).await;
        tell(
            report,
            out,
            &what,
            &frames,
            (&window_now, client, (0, 1000, 1000)),
            (&panel_rest, rect, (1000, 0, 0)),
            badge,
        );
        drop(frames);
        let _ = writeln!(
            report,
            "{what}: window open {}, panel open {}",
            cx.update(|cx| view::window_open(cx)).map_err(gone)?,
            cx.update(|cx| panel::is_open(cx)).map_err(gone)?
        );
    }
    let _ = window_now;
    cx.update(panel::slide_away).map_err(gone)?;
    pause(800, cx).await;
    crate::frames::mute(false);
    Ok(())
}

/// Writes what the log says of emoji glyphs since it was last taken (there
/// should be none), and empties it.
fn gear_log(report: &mut String, what: &str) {
    for line in crate::flip::take_log() {
        if line.contains("emoji") || line.contains("big window:") {
            let _ = writeln!(report, "{what}: log {line}");
        }
    }
}

/// Waits for the flip under way to end.
async fn flip_ends(cx: &mut AsyncApp) -> Result<(), String> {
    let began = Instant::now();
    pause(50, cx).await;
    loop {
        let running = cx
            .update(|cx| crate::flip::running(cx))
            .map_err(|_| "the app went away".to_string())?;
        if !running {
            return Ok(());
        }
        if began.elapsed() > Duration::from_secs(8) {
            return Err("the flip did not end in 8 s".into());
        }
        pause(10, cx).await;
    }
}

/// How many px differ between `a` and `b` by more than a little, over `r`
/// (screen px, cut to what both show).
fn changed(a: &Shot, b: &Shot, r: Rect) -> u32 {
    let r = Rect::new(
        r.left.max(a.rect.left).max(b.rect.left),
        r.top.max(a.rect.top).max(b.rect.top),
        r.right.min(a.rect.right).min(b.rect.right),
        r.bottom.min(a.rect.bottom).min(b.rect.bottom),
    );
    if r.width() <= 0 || r.height() <= 0 {
        return 0;
    }
    fn row(s: &Shot, r: Rect, y: i32) -> Option<&[u8]> {
        let from = ((y - s.rect.top) as usize * s.w as usize + (r.left - s.rect.left) as usize) * 4;
        s.pixels.get(from..from + r.width() as usize * 4)
    }
    let mut n = 0;
    for y in r.top..r.bottom {
        let (Some(ra), Some(rb)) = (row(a, r, y), row(b, r, y)) else {
            continue;
        };
        // Mostly the same: whole rows at once.
        if ra == rb {
            continue;
        }
        for (p, q) in ra.as_chunks::<4>().0.iter().zip(rb.as_chunks::<4>().0) {
            if p[0].abs_diff(q[0]) > 2 || p[1].abs_diff(q[1]) > 2 || p[2].abs_diff(q[2]) > 2 {
                n += 1;
            }
        }
    }
    n
}

/// The screen, taken over and over on a thread of its own while a flip
/// runs, each picture with the flip's frame drawn last.
struct Watch {
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<Vec<Caught>>,
}

struct Caught {
    ms: u128,
    drawn: Option<(u16, u16, u16)>,
    shot: Shot,
}

/// No more pictures than this are kept of one flip.
const MOST: usize = 110;

impl Watch {
    /// Starts taking `rect`; the pictures also go in `folder`.
    fn start(rect: Rect, folder: PathBuf) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            let _ = std::fs::create_dir_all(&folder);
            let began = Instant::now();
            let mut caught = Vec::new();
            while !stopped.load(Ordering::Relaxed) && caught.len() < MOST {
                let before = crate::flip::drawn();
                let Some((w, _, pixels)) = win::capture(rect) else {
                    break;
                };
                // Only pictures the flip did not draw a new frame under.
                let drawn = crate::flip::drawn();
                caught.push(Caught {
                    ms: began.elapsed().as_millis(),
                    drawn: if before == drawn { drawn } else { before },
                    shot: Shot { rect, w, pixels },
                });
            }
            // Every third, and the ends: saving them all takes too long.
            for (i, c) in caught.iter().enumerate() {
                if i % 3 != 0 && i + 1 != caught.len() {
                    continue;
                }
                let h = c.shot.pixels.len() as u32 / 4 / c.shot.w.max(1);
                let name = match c.drawn {
                    Some((p, t, w)) => format!("{i:03}-{:05}ms-p{p}-t{t}-w{w}.png", c.ms),
                    None => format!("{i:03}-{:05}ms-rest.png", c.ms),
                };
                let Ok(file) = std::fs::File::create(folder.join(name)) else {
                    continue;
                };
                let png = PngEncoder::new_with_quality(
                    std::io::BufWriter::new(file),
                    CompressionType::Fast,
                    FilterType::NoFilter,
                );
                let _ =
                    png.write_image(&c.shot.pixels, c.shot.w, h, image::ExtendedColorType::Rgba8);
            }
            caught
        });
        Self { stop, thread }
    }

    /// Stops, and hands over what it caught once its pictures are saved.
    async fn stop(self, cx: &mut AsyncApp) -> Vec<Caught> {
        self.stop.store(true, Ordering::Relaxed);
        while !self.thread.is_finished() {
            pause(50, cx).await;
        }
        self.thread.join().unwrap_or_default()
    }
}

/// The box around the px that differ between `a` and `b` inside `r`.
fn unlike_box(a: &Shot, b: &Shot, r: Rect) -> Option<Rect> {
    let mut found: Option<Rect> = None;
    for y in r.top..r.bottom {
        let mut row: Option<(i32, i32)> = None;
        for x in r.left..r.right {
            let (p, q) = (a.at(x, y), b.at(x, y));
            if (0..3).any(|c| p[c].abs_diff(q[c]) > 2) {
                row = Some(row.map_or((x, x), |(l, _)| (l, x)));
            }
        }
        if let Some((l, right)) = row {
            found = Some(found.map_or(Rect::new(l, y, right + 1, y + 1), |f| {
                Rect::new(f.left.min(l), f.top, f.right.max(right + 1), y + 1)
            }));
        }
    }
    found
}

/// Where `a` and `b` differ inside `r`, as lines for the report: the 48 px
/// squares with the most px that differ, each with where its first such px
/// is and both colours there. Also saved as `name`.png in `out`: `r` with
/// the px that differ white on black.
fn unlike_cells(out: &std::path::Path, name: &str, a: &Shot, b: &Shot, r: Rect) -> Vec<String> {
    const CELL: i32 = 48;
    let (w, h) = (r.width().max(0), r.height().max(0));
    let cols = (w + CELL - 1) / CELL;
    let mut cells = std::collections::BTreeMap::new();
    let mut mask = vec![0u8; (w * h) as usize];
    for y in r.top..r.bottom {
        for x in r.left..r.right {
            let (p, q) = (a.at(x, y), b.at(x, y));
            if (0..3).any(|c| p[c].abs_diff(q[c]) > 2) {
                mask[((y - r.top) * w + (x - r.left)) as usize] = 255;
                let cell = ((y - r.top) / CELL, (x - r.left) / CELL);
                cells.entry(cell).or_insert((0u32, (x, y), p, q)).0 += 1;
            }
        }
    }
    let _ = image::save_buffer(
        out.join(format!("{name}.png")),
        &mask,
        w as u32,
        h as u32,
        image::ExtendedColorType::L8,
    );
    let mut most: Vec<_> = cells.into_iter().collect();
    let squares = most.len();
    most.sort_by_key(|(_, (n, ..))| std::cmp::Reverse(*n));
    let mut lines = vec![format!(
        "{squares} of {} squares of {CELL} px differ; the most:",
        cols * ((h + CELL - 1) / CELL)
    )];
    for (i, ((row, col), (n, at, p, q))) in most.into_iter().take(30).enumerate() {
        let (left, top) = (r.left + col * CELL, r.top + row * CELL);
        lines.push(format!(
            "square {i} at ({left}, {top}): {n} px, first at {at:?} flip {p:?} rest {q:?}"
        ));
        // The first few, eight times as large: the flip's beside the
        // view's.
        if i < 10 {
            let square = Rect::new(left - 8, top - 8, left + CELL + 8, top + CELL + 8);
            save_zoomed(out, &format!("{name}-square-{i}"), a, b, square);
        }
        // A few px around the first, as numbers (each px's red, in hex):
        // the flip's rows, then the view's.
        if n < 200 && lines.len() < 150 {
            for (who, shot) in [("flip", a), ("rest", b)] {
                for y in at.1 - 2..at.1 + 8 {
                    let row: Vec<String> = (at.0 - 4..at.0 + 12)
                        .map(|x| {
                            let inside = x >= shot.rect.left
                                && x < shot.rect.right
                                && y >= shot.rect.top
                                && y < shot.rect.bottom;
                            if inside {
                                format!("{:02x}", shot.at(x, y)[0])
                            } else {
                                "--".into()
                            }
                        })
                        .collect();
                    lines.push(format!("  {who} y{y}: {}", row.join(" ")));
                }
            }
        }
    }
    lines
}

/// Saves `r` of `a` (left) and of `b` (right) side by side as `name`.png
/// in `out`, each px eight px wide.
fn save_zoomed(out: &std::path::Path, name: &str, a: &Shot, b: &Shot, r: Rect) {
    const ZOOM: i32 = 8;
    const GAP: i32 = 16;
    let (w, h) = (r.width().max(0), r.height().max(0));
    let (wide, high) = (w * ZOOM * 2 + GAP, h * ZOOM);
    let inside = |s: &Shot, x: i32, y: i32| {
        x >= s.rect.left && x < s.rect.right && y >= s.rect.top && y < s.rect.bottom
    };
    let mut pixels = Vec::with_capacity((wide * high * 3) as usize);
    for y in 0..high {
        for x in 0..wide {
            let (shot, x) = if x < w * ZOOM {
                (Some(a), x)
            } else if x >= w * ZOOM + GAP {
                (Some(b), x - w * ZOOM - GAP)
            } else {
                (None, 0)
            };
            let (sx, sy) = (r.left + x / ZOOM, r.top + y / ZOOM);
            let p = match shot {
                Some(s) if inside(s, sx, sy) => s.at(sx, sy),
                _ => [255, 0, 255],
            };
            pixels.extend_from_slice(&p);
        }
    }
    let _ = image::save_buffer(
        out.join(format!("{name}.png")),
        &pixels,
        wide as u32,
        high as u32,
        image::ExtendedColorType::Rgb8,
    );
}

/// A view at rest: its picture, where it is, and the flip's frame that
/// must look the same.
type Rest<'a> = (&'a Shot, Rect, (u16, u16, u16));

/// Writes what the pictures of a flip show: for each, how many px differ
/// from the picture before it (over the whole screen), from the view it
/// started from and from the view it ends in (each inside that view).
/// Where a picture is of the frame that must look like a view at rest and
/// doesn't, also where they differ, and both saved there.
fn tell(
    report: &mut String,
    out: &std::path::Path,
    what: &str,
    frames: &[Caught],
    from: Rest,
    to: Rest,
    badge: Option<Rect>,
) {
    // The badge shows over both views and over the flip: its px never
    // change.
    if let (Some(badge), Some(first)) = (badge, frames.first()) {
        let inner = badge.outset(-badge.width() / 5);
        let changed: Vec<String> = frames
            .iter()
            .filter(|c| changed(&c.shot, &first.shot, inner) > 0)
            .map(|c| format!("{} ms", c.ms))
            .collect();
        let _ = writeln!(
            report,
            "{what}: the badge changed in {} of {} pictures (must be 0): {}",
            changed.len(),
            frames.len(),
            changed.join(", ")
        );
    }
    for line in crate::flip::take_log() {
        let _ = writeln!(report, "{what}: log {line}");
    }
    let _ = writeln!(
        report,
        "{what}: {} pictures; each: ms, frame drawn (panel/turn/window, thousandths), px changed \
         since the one before, px unlike the start inside {:?}, px unlike the end inside {:?}",
        frames.len(),
        from.1,
        to.1
    );
    let mut before: Option<&Shot> = None;
    let mut saved = [false; 2];
    for (i, c) in frames.iter().enumerate() {
        let drawn = match c.drawn {
            Some((p, t, w)) => format!("{p}/{t}/{w}"),
            None => "rest".into(),
        };
        let unlike = [
            changed(&c.shot, from.0, from.1),
            changed(&c.shot, to.0, to.1),
        ];
        let _ = write!(
            report,
            "{what}: {:5} {drawn:>14} {:8} {:8} {:8}",
            c.ms,
            before.map_or(0, |b| changed(&c.shot, b, c.shot.rect)),
            unlike[0],
            unlike[1],
        );
        for (k, rest) in [from, to].into_iter().enumerate() {
            if c.drawn == Some(rest.2)
                && unlike[k] > 0
                && let Some(b) = unlike_box(&c.shot, rest.0, rest.1)
            {
                let _ = write!(report, "  unlike its view at rest inside {b:?}");
                if !saved[k] {
                    saved[k] = true;
                    let name = what.replace(' ', "-");
                    let end = ["start", "end"][k];
                    let mask = format!("{name}-unlike-{end}-{i:03}-where");
                    for line in unlike_cells(out, &mask, &c.shot, rest.0, rest.1) {
                        let _ = write!(report, "\n{what}:     {line}");
                    }
                }
            }
        }
        let _ = writeln!(report);
        before = Some(&c.shot);
    }
}
