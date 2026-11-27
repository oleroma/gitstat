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
use gpui::{App, AsyncApp};
use std::fmt::Write as _;
use std::path::PathBuf;
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
            .timer(Duration::from_millis(1500))
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

/// Saves the screen inside `rect` as `name`.png in `out`, off this thread.
fn still(out: &std::path::Path, name: &str, rect: Rect) {
    let file = out.join(format!("{name}.png"));
    std::thread::spawn(move || {
        if let Some((w, h, pixels)) = win::capture(rect) {
            let _ = image::save_buffer(file, &pixels, w, h, image::ExtendedColorType::Rgba8);
        }
    });
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
    let opened = Instant::now();
    cx.update(|cx| {
        if let Some(r) = panel::guess_place(cx) {
            crate::frames::crop(Some(around(r)));
        }
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
    let _ = writeln!(report, "panel: shown after {shown_after:?}\n{facts}");
    let rect = cx.update(|cx| panel::last_rect(cx)).map_err(gone)?;
    let Some(rect) = rect else {
        return Err("the panel has no place".into());
    };
    if shown_after.is_none() {
        return Err("the panel never showed".into());
    }
    still(out, "panel-rest", around(rect));
    pause(300, cx).await;
    // Twice the size, for reading its pixels.
    still(out, "panel-only", rect.outset((12.0 * mon.scale) as i32));
    pause(300, cx).await;
    cx.update(|cx| {
        crate::frames::crop(Some(around(rect)));
        panel::slide_away(cx);
    })
    .map_err(gone)?;
    pause(1200, cx).await;
    let open = cx.update(|cx| panel::is_open(cx)).map_err(gone)?;
    let _ = writeln!(report, "panel: open after sliding away: {open}");
    still(out, "panel-gone", around(rect));
    pause(300, cx).await;
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
    let _ = writeln!(report, "window: client {client:?}");
    still(out, "window-rest", mon.rect);
    pause(500, cx).await;
    cx.update(view::close_window).map_err(gone)?;
    pause(500, cx).await;
    Ok(())
}
