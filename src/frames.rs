//! A recorder for checking the flip by eye, frame by frame: with config's
//! `record_frames` on (Settings › Diagnostics), each flip saves the screen
//! under it on every frame it draws, and for a moment after it closes, as
//! numbered PNGs in a folder of their own under `Config::frames_folder`.
//! Off by default. The screen is captured and saved on threads of their
//! own, but grabbing it every frame still slows the flip.

use crate::layout::Rect;
use std::cell::RefCell;
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
    /// The part of the screen recorded instead of what a recording asks
    /// for: the self-test's, around the view it is testing.
    static CROP: std::cell::Cell<Option<Rect>> = const { std::cell::Cell::new(None) };
    static MUTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Records nothing from now on (or again): the self-test's flips, which it
/// takes its own pictures of.
pub fn mute(on: bool) {
    MUTED.set(on);
}

/// Records only `rect` (physical px) from now on, whatever a recording
/// asks for; None, what it asks for again.
pub fn crop(rect: Option<Rect>) {
    CROP.set(rect);
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
    let rect = CROP.get().unwrap_or(rect);
    let config = &ui.state.read(cx).config;
    if !config.record_frames || MUTED.get() {
        RECORDER.with(|r| r.borrow_mut().take());
        return;
    }
    let folder = config.frames_folder();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let dir = folder.join(format!("{name}-{stamp}"));
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    // One thread captures each frame as soon as it's asked for; another
    // writes them, however long that takes.
    let (tx, rx) = mpsc::channel::<String>();
    let (save, saved) = mpsc::channel::<(String, u32, u32, Vec<u8>)>();
    std::thread::spawn(move || {
        while let Ok(mut name) = rx.recv() {
            // Behind: only the newest frame asked for, so each picture is
            // the moment its name says.
            while let Ok(newer) = rx.try_recv() {
                name = newer;
            }
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
    // Only this flip's recording: a flip starting meanwhile records its
    // own, which this leaves alone.
    let mine = RECORDER.with(|r| r.borrow().as_ref().map(|r| r.start));
    let still = move || RECORDER.with(|r| r.borrow().as_ref().map(|r| r.start)) == mine;
    cx.spawn(async move |cx| {
        let end = Instant::now() + AFTER;
        while Instant::now() < end && still() {
            cx.background_executor().timer(AFTER_EVERY).await;
            if still() {
                frame("after");
            }
        }
        if still() {
            RECORDER.with(|r| r.borrow_mut().take());
        }
    })
    .detach();
}
