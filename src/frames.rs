//! A recorder for checking the flip by eye, frame by frame: with config's
//! `flip_frames` set to a folder, each flip saves the screen under it on
//! every frame it draws, and for a moment after it closes, as numbered
//! PNGs in a folder of their own. Off by default and not in the settings
//! window. The screen is captured and saved on a thread of its own, so
//! recording barely slows the flip.

use crate::layout::Rect;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long recording goes on after the flip closes, and how often.
const AFTER: Duration = Duration::from_millis(250);
const AFTER_EVERY: Duration = Duration::from_millis(16);

struct Recorder {
    start: Instant,
    n: u32,
    /// To the thread that writes the files.
    tx: mpsc::Sender<String>,
}

thread_local! {
    static RECORDER: RefCell<Option<Recorder>> = const { RefCell::new(None) };
}

/// Starts recording `rect` (physical px), when config asks for it.
pub fn start(rect: Rect, cx: &gpui::App) {
    start_named(rect, "flip", cx);
}

/// Records `rect` (physical px) every few ms for a moment, when config
/// asks for it and nothing else is being recorded: to catch what shows as
/// the panel opens.
pub fn watch(rect: Rect, what: &'static str, cx: &mut gpui::App) {
    if RECORDER.with(|r| r.borrow().is_some()) {
        return;
    }
    start_named(rect, what, cx);
    if RECORDER.with(|r| r.borrow().is_none()) {
        return;
    }
    frame("start");
    let mine = RECORDER.with(|r| r.borrow().as_ref().map(|r| r.start));
    // Stops early when a flip starts recording in its place.
    let still = move || RECORDER.with(|r| r.borrow().as_ref().map(|r| r.start)) == mine;
    cx.spawn(async move |cx| {
        let end = Instant::now() + WATCH;
        while Instant::now() < end && still() {
            cx.background_executor().timer(AFTER_EVERY).await;
            if still() {
                frame(what);
            }
        }
        if still() {
            RECORDER.with(|r| r.borrow_mut().take());
        }
    })
    .detach();
}

/// How long [`watch`] records.
const WATCH: Duration = Duration::from_millis(700);

fn start_named(rect: Rect, name: &str, cx: &gpui::App) {
    let Some(ui) = cx.try_global::<crate::view::Ui>() else {
        return;
    };
    let folder = ui.state.read(cx).config.flip_frames.trim().to_string();
    if folder.is_empty() {
        RECORDER.with(|r| r.borrow_mut().take());
        return;
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let dir = PathBuf::from(folder).join(format!("{name}-{stamp}"));
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    // One thread captures each frame as soon as it's asked for; another
    // writes them, however long that takes.
    let (tx, rx) = mpsc::channel::<String>();
    let (save, saved) = mpsc::channel::<(String, u32, u32, Vec<u8>)>();
    std::thread::spawn(move || {
        for name in rx {
            if let Some((w, h, pixels)) = crate::win::capture(rect) {
                let _ = save.send((name, w, h, pixels));
            }
        }
    });
    std::thread::spawn(move || {
        for (name, w, h, pixels) in saved {
            let _ = image::save_buffer(
                dir.join(name),
                &pixels,
                w,
                h,
                image::ExtendedColorType::Rgba8,
            );
        }
    });
    RECORDER.with(|r| {
        *r.borrow_mut() = Some(Recorder {
            start: Instant::now(),
            n: 0,
            tx,
        });
    });
}

/// Saves the screen as it is now, if recording; `what` names the moment.
pub fn frame(what: &str) {
    RECORDER.with(|r| {
        let mut r = r.borrow_mut();
        let Some(rec) = r.as_mut() else {
            return;
        };
        rec.n += 1;
        let ms = rec.start.elapsed().as_millis();
        let name = format!("{:04}-{ms:05}ms-{what}.png", rec.n);
        let _ = rec.tx.send(name);
    });
}

/// The flip has closed: records a moment longer, then stops.
pub fn finish(cx: &mut gpui::App) {
    if RECORDER.with(|r| r.borrow().is_none()) {
        return;
    }
    cx.spawn(async move |cx| {
        let end = Instant::now() + AFTER;
        while Instant::now() < end {
            cx.background_executor().timer(AFTER_EVERY).await;
            frame("after");
        }
        RECORDER.with(|r| r.borrow_mut().take());
    })
    .detach();
}
