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
        let verdict = run(&out, &mut report, cx).await;
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
    let before = shot(out, "panel-before", around(place), cx).await;
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

    // The flip: panel to big window and back, every frame that can be
    // caught, against the views at rest.
    let window_rest = whole;
    cx.update(|cx| panel::toggle(anchor, cx)).map_err(gone)?;
    pause(1500, cx).await;
    let panel_rest = shot(out, "flip-panel-rest", mon.rect, cx).await;
    let (Some(panel_rest), Some(window_rest)) = (panel_rest, window_rest) else {
        return Err("no picture of the screen".into());
    };
    let (panel_rest, window_rest) = (Arc::new(panel_rest), Arc::new(window_rest));
    let watch = Watch::start(mon.rect, out.join("flip-open"));
    let asked = cx
        .update(|cx| {
            let handle = cx
                .try_global::<panel::PanelSlot>()
                .and_then(|s| s.handle())?;
            handle
                .update(cx, |_, window, cx| {
                    crate::flip::open_window(window, None, cx)
                })
                .ok()
        })
        .map_err(gone)?;
    if asked.is_none() {
        return Err("no panel to flip from".into());
    }
    flip_ends(cx).await?;
    pause(400, cx).await;
    let frames = watch.stop(cx).await;
    let open = cx.update(|cx| view::window_open(cx)).map_err(gone)?;
    let now = cx
        .update(|cx| view::window_hwnd(cx).and_then(win::client_rect))
        .map_err(gone)?;
    let _ = writeln!(
        report,
        "flip open: window open {open}, client {now:?} (was {client:?}), panel open {}",
        cx.update(|cx| panel::is_open(cx)).map_err(gone)?
    );
    tell(
        report,
        "flip open",
        &frames,
        &panel_rest,
        rect,
        &window_rest,
        client,
    );
    if !open {
        return Err("the flip did not open the big window".into());
    }
    pause(600, cx).await;
    let Some(window_now) = shot(out, "flip-window-rest", mon.rect, cx).await else {
        return Err("no picture of the screen".into());
    };
    let _ = writeln!(
        report,
        "flip open: the window it ended in against the window opened plainly, px that differ \
         inside it: {}",
        changed(&window_now, &window_rest, client)
    );
    let window_now = Arc::new(window_now);
    let watch = Watch::start(mon.rect, out.join("flip-close"));
    let asked = cx
        .update(|cx| {
            let handle = view::window_handle(cx)?;
            handle
                .update(cx, |_, window, cx| crate::flip::flip_back(window, cx))
                .ok()
        })
        .map_err(gone)?;
    let _ = writeln!(report, "flip close: taken by the flip: {asked:?}");
    if asked != Some(true) {
        return Err("the flip back did not start".into());
    }
    flip_ends(cx).await?;
    pause(1500, cx).await;
    let frames = watch.stop(cx).await;
    tell(
        report,
        "flip close",
        &frames,
        &window_now,
        client,
        &panel_rest,
        rect,
    );
    let _ = writeln!(
        report,
        "flip close: window open {}, panel open {}",
        cx.update(|cx| view::window_open(cx)).map_err(gone)?,
        cx.update(|cx| panel::is_open(cx)).map_err(gone)?
    );
    Ok(())
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
const MOST: usize = 90;

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

/// Writes what the pictures of a flip show: for each, how many px differ
/// from the picture before it (over the whole screen), from the view it
/// started from (inside `from_in`) and from the view it ends in (inside
/// `to_in`).
fn tell(
    report: &mut String,
    what: &str,
    frames: &[Caught],
    from: &Shot,
    from_in: Rect,
    to: &Shot,
    to_in: Rect,
) {
    let _ = writeln!(
        report,
        "{what}: {} pictures; each: ms, frame drawn (panel/turn/window, thousandths), px changed \
         since the one before, px unlike the start inside {from_in:?}, px unlike the end inside \
         {to_in:?}",
        frames.len()
    );
    let mut before: Option<&Shot> = None;
    for c in frames {
        let drawn = match c.drawn {
            Some((p, t, w)) => format!("{p}/{t}/{w}"),
            None => "rest".into(),
        };
        let _ = writeln!(
            report,
            "{what}: {:5} {drawn:>14} {:8} {:8} {:8}",
            c.ms,
            before.map_or(0, |b| changed(&c.shot, b, c.shot.rect)),
            changed(&c.shot, from, from_in),
            changed(&c.shot, to, to_in),
        );
        before = Some(&c.shot);
    }
}
