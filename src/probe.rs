//! Measurements for the animation work, run from Settings › Diagnostics.
//! Nothing here is part of the app's own behaviour: it opens two test
//! windows for a few seconds, measures, writes a report into the
//! recordings folder and closes them.
//!
//! 1. The swap: two windows showing the same picture at the same place,
//!    one cloaked. One is cloaked and the other uncloaked back to back,
//!    and the screen under their shadow is captured as fast as it can be
//!    for a moment after. If both changes land in one screen refresh, no
//!    capture differs from the picture before. Two controls, which leave a
//!    refresh between the two changes on purpose, show that a one-refresh
//!    gap or overlap would be caught.
//! 2. The bends: a few hundred curves drawn afresh by tiny-skia every
//!    frame, each in its own tile of one small picture, and the last
//!    picture dropped, to see what crisp bends in every frame of a turn
//!    would cost. (A window-sized picture per frame was too slow: 13 ms.)

use crate::layout::Rect;
use crate::view::Palette;
use crate::win;
use gpui::{
    App, AppContext, Bounds, Context, IntoElement, ParentElement, Render, RenderImage, Styled,
    Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions,
    canvas, div, point, px, quad, size,
};
use resvg::tiny_skia;
use std::cell::Cell;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Swaps per way of swapping.
const SWAPS: usize = 30;
/// How long the screen is watched after each swap.
const WATCH: Duration = Duration::from_millis(70);
/// Frames of bends drawn.
const BEND_FRAMES: usize = 120;
/// Curves per frame: about what a panel full of busy trees has.
const BENDS: usize = 300;
/// Room around the test card for its shadow, logical px.
const ROOM: f32 = 72.0;

thread_local! {
    static RUNNING: Cell<bool> = const { Cell::new(false) };
}

struct Bends {
    /// How long each frame's picture took to draw.
    raster: Vec<Duration>,
    /// When each frame was drawn.
    at: Vec<Instant>,
    image: Option<Arc<RenderImage>>,
    old: Option<Arc<RenderImage>>,
}

struct ProbeView {
    bends: Option<Bends>,
}

impl ProbeView {
    fn bends_done(&self) -> bool {
        self.bends
            .as_ref()
            .is_some_and(|b| b.at.len() >= BEND_FRAMES)
    }
}

/// A bend's tile in the packed picture, device px, and tiles per row.
const TILE: u32 = 48;
const COLS: u32 = 20;
/// Room in a tile around its bend.
const INSET: f32 = 4.0;

/// Where bend `i` sits `n` frames in, device px in a `fw` by `fh` window,
/// and how far it runs across and down: each a little further turned, as
/// they would be in a flip.
fn bend_at(i: usize, n: usize, fw: f32, fh: f32) -> (f32, f32, f32, f32) {
    let k = i as f32 / BENDS as f32;
    let turn = n as f32 / BEND_FRAMES as f32;
    let x = fw * (0.08 + 0.84 * ((k * 7.0 + turn * 0.3) % 1.0));
    let y = fh * (0.1 + 0.8 * ((k * 13.0) % 1.0));
    (
        x.round(),
        y.round(),
        40.0 * (1.0 - turn) + 12.0 * turn,
        12.0 * (1.0 - turn) + 40.0 * turn,
    )
}

/// One frame's bends, `n` frames in, each drawn in its own tile of one
/// small picture.
fn draw_bends(w: u32, h: u32, n: usize) -> Option<Arc<RenderImage>> {
    let rows = (BENDS as u32).div_ceil(COLS);
    let mut pixmap = tiny_skia::Pixmap::new(COLS * TILE, rows * TILE)?;
    let stroke = tiny_skia::Stroke {
        width: 2.0,
        line_cap: tiny_skia::LineCap::Round,
        ..Default::default()
    };
    for i in 0..BENDS {
        let (_, _, dx, dy) = bend_at(i, n, w as f32, h as f32);
        let x = (i as u32 % COLS * TILE) as f32 + INSET;
        let y = (i as u32 / COLS * TILE) as f32 + INSET;
        let mut path = tiny_skia::PathBuilder::new();
        path.move_to(x, y);
        path.cubic_to(x + dx / 2.0, y, x + dx / 2.0, y + dy, x + dx, y + dy);
        let Some(path) = path.finish() else { continue };
        let mut paint = tiny_skia::Paint::default();
        paint.set_color_rgba8(90 + (i % 3) as u8 * 60, 140, 255 - (i % 5) as u8 * 30, 255);
        paint.anti_alias = true;
        pixmap.stroke_path(
            &path,
            &paint,
            &stroke,
            tiny_skia::Transform::identity(),
            None,
        );
    }
    crate::timeline::ink::to_image(pixmap)
}

impl Render for ProbeView {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_appearance(window.appearance());
        let s = window.scale_factor();
        let view = window.viewport_size();
        let (w, h) = (
            (f32::from(view.width) * s).round() as u32,
            (f32::from(view.height) * s).round() as u32,
        );
        let mut images = (None, None);
        let frame = self.bends.as_ref().map_or(0, |b| b.at.len());
        if let Some(b) = self.bends.as_mut()
            && b.at.len() < BEND_FRAMES
        {
            let start = Instant::now();
            let image = draw_bends(w, h, b.at.len());
            b.raster.push(start.elapsed());
            b.at.push(Instant::now());
            b.old = std::mem::replace(&mut b.image, image);
            images = (b.image.clone(), b.old.take());
            window.request_animation_frame();
        } else if let Some(b) = self.bends.as_ref() {
            images = (b.image.clone(), None);
        }
        let (image, old) = images;
        div().size_full().child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    let card = Bounds::new(
                        point(bounds.left() + px(ROOM), bounds.top() + px(ROOM)),
                        size(
                            bounds.size.width - px(2.0 * ROOM),
                            bounds.size.height - px(2.0 * ROOM),
                        ),
                    );
                    window.paint_shadows(
                        card,
                        gpui::Corners::all(px(win::PANEL_RADIUS)),
                        &crate::scene::parts::window_shadow(),
                    );
                    window.paint_quad(quad(
                        card,
                        px(win::PANEL_RADIUS),
                        p.bg,
                        px(1.0),
                        p.border,
                        gpui::BorderStyle::Solid,
                    ));
                    if let Some(old) = old {
                        window.drop_image(old).ok();
                    }
                    // Each bend's tile, 1:1 on whole device px where its
                    // bend goes: the whole picture placed so that tile
                    // lands there, masked to the tile.
                    if let Some(image) = image {
                        let rows = (BENDS as u32).div_ceil(COLS);
                        let whole =
                            size(px((COLS * TILE) as f32 / s), px((rows * TILE) as f32 / s));
                        let tile = size(px(TILE as f32 / s), px(TILE as f32 / s));
                        for i in 0..BENDS {
                            let (x, y, _, _) = bend_at(i, frame, w as f32, h as f32);
                            let at = point(
                                bounds.left() + px((x - INSET) / s),
                                bounds.top() + px((y - INSET) / s),
                            );
                            let origin = point(
                                at.x - px((i as u32 % COLS * TILE) as f32 / s),
                                at.y - px((i as u32 / COLS * TILE) as f32 / s),
                            );
                            window.with_content_mask(
                                Some(gpui::ContentMask {
                                    bounds: Bounds::new(at, tile),
                                }),
                                |window| {
                                    window
                                        .paint_image(
                                            Bounds::new(origin, whole),
                                            Default::default(),
                                            image.clone(),
                                            0,
                                            false,
                                        )
                                        .ok();
                                },
                            );
                        }
                    }
                },
            )
            .size_full(),
        )
    }
}

/// A way of swapping the two windows.
#[derive(Clone, Copy)]
enum Way {
    /// After a screen refresh: cloak the shown one, uncloak the other.
    FlushCloakUncloak,
    /// After a screen refresh: uncloak the other, cloak the shown one.
    FlushUncloakCloak,
    /// Whenever: cloak the shown one, uncloak the other.
    CloakUncloak,
    /// Control: a refresh between cloak and uncloak, so neither shows.
    GapControl,
    /// Control: a refresh between uncloak and cloak, so both show.
    OverlapControl,
}

const WAYS: [(&str, Way); 5] = [
    ("refresh, cloak, uncloak", Way::FlushCloakUncloak),
    ("refresh, uncloak, cloak", Way::FlushUncloakCloak),
    ("cloak, uncloak (no wait)", Way::CloakUncloak),
    ("control: gap of one refresh", Way::GapControl),
    ("control: overlap of one refresh", Way::OverlapControl),
];

fn swap(way: Way, shown: isize, hidden: isize) {
    match way {
        Way::FlushCloakUncloak => {
            win::dwm_flush();
            win::cloak(shown);
            win::uncloak(hidden);
        }
        Way::FlushUncloakCloak => {
            win::dwm_flush();
            win::uncloak(hidden);
            win::cloak(shown);
        }
        Way::CloakUncloak => {
            win::cloak(shown);
            win::uncloak(hidden);
        }
        Way::GapControl => {
            win::dwm_flush();
            win::cloak(shown);
            win::dwm_flush();
            win::dwm_flush();
            win::uncloak(hidden);
        }
        Way::OverlapControl => {
            win::dwm_flush();
            win::uncloak(hidden);
            win::dwm_flush();
            win::dwm_flush();
            win::cloak(shown);
        }
    }
}

/// How a capture differs from the picture before the swaps: how many
/// pixels, and whether it is darker (both windows' shadows) or lighter
/// (neither's) on the whole.
fn differs(reference: &[u8], now: &[u8]) -> (usize, i64) {
    let mut count = 0;
    let mut luma = 0i64;
    for (a, b) in reference
        .as_chunks::<4>()
        .0
        .iter()
        .zip(now.as_chunks::<4>().0)
    {
        let d = (0..3).map(|i| (a[i] as i32 - b[i] as i32).abs()).max();
        if d.unwrap_or(0) > 6 {
            count += 1;
            luma += (0..3).map(|i| b[i] as i64 - a[i] as i64).sum::<i64>();
        }
    }
    (count, luma)
}

fn open(rect: Rect, cx: &mut App) -> Option<(WindowHandle<ProbeView>, isize)> {
    let mut hwnd = None;
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(crate::panel::logical(rect))),
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
                hwnd = win::hwnd(window);
                if let Some(h) = hwnd {
                    win::cloak(h);
                }
                cx.new(|_| ProbeView { bends: None })
            },
        )
        .ok()?;
    Some((handle, hwnd?))
}

/// Runs the probes and writes `probe-<time>.txt` (and a picture of the
/// bends' last frame) into `folder`.
pub fn run(cx: &mut App) {
    if RUNNING.replace(true) {
        return;
    }
    let Some(folder) = cx
        .try_global::<crate::view::Ui>()
        .map(|ui| ui.state.read(cx).config.frames_folder())
    else {
        RUNNING.set(false);
        return;
    };
    // As big as a big window, where the monitor has the room.
    let mon = win::primary_monitor();
    let work = mon.work;
    let (w, h) = (
        (work.width() - 80).min(2000),
        (work.height() - 80).min(1200),
    );
    let (cx_, cy_) = work.centre();
    let rect = Rect::new(cx_ - w / 2, cy_ - h / 2, cx_ - w / 2 + w, cy_ - h / 2 + h);
    let (Some((a, ha)), Some((b, hb))) = (open(rect, cx), open(rect, cx)) else {
        RUNNING.set(false);
        return;
    };
    // The card's bottom edge and the deepest of its shadow, below it.
    let room = (ROOM * mon.scale).round() as i32;
    let strip = Rect::new(
        cx_ - 150,
        rect.bottom - room - 20,
        cx_ + 150,
        rect.bottom - room + 100,
    );
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_millis(60)).await;
        unsafe {
            win::style_morph(ha);
            win::style_morph(hb);
            win::place_morph(ha, rect);
            win::place_morph(hb, rect);
        }
        // Both drawn, cloaked.
        cx.background_executor().timer(Duration::from_millis(600)).await;
        let mut report = String::new();
        let _ = writeln!(
            report,
            "gitstat probes\nscale {} window {}x{} px strip {}x{} px",
            mon.scale,
            rect.width(),
            rect.height(),
            strip.width(),
            strip.height()
        );
        win::uncloak(ha);
        cx.background_executor().timer(Duration::from_millis(300)).await;
        let reference = win::capture(strip).map(|c| c.2);
        let (mut shown, mut hidden) = (ha, hb);
        if let Some(reference) = reference.map(Arc::new) {
            let _ = writeln!(report, "\nSWAP: {SWAPS} swaps each, screen watched {WATCH:?} after each");
            for (name, way) in WAYS {
                let (mut captures, mut bad, mut darker, mut lighter, mut worst) = (0, 0, 0, 0, 0);
                let mut bad_swaps = 0;
                let mut spent = Duration::ZERO;
                for _ in 0..SWAPS {
                    // Watched from before the swap until a moment after
                    // it, on a thread of its own, so nothing in between
                    // is missed.
                    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let (stopped, reference) = (stop.clone(), reference.clone());
                    let watcher = std::thread::spawn(move || {
                        let mut seen = (0u32, 0u32, 0u32, 0u32, 0usize, Duration::ZERO);
                        while !stopped.load(std::sync::atomic::Ordering::Relaxed) {
                            let t = Instant::now();
                            let Some((_, _, now)) = win::capture(strip) else {
                                break;
                            };
                            seen.5 += t.elapsed();
                            seen.0 += 1;
                            let (count, luma) = differs(&reference, &now);
                            if count > 0 {
                                seen.1 += 1;
                                seen.4 = seen.4.max(count);
                                if luma < 0 {
                                    seen.2 += 1;
                                } else {
                                    seen.3 += 1;
                                }
                            }
                        }
                        seen
                    });
                    cx.background_executor()
                        .timer(Duration::from_millis(20))
                        .await;
                    swap(way, shown, hidden);
                    std::mem::swap(&mut shown, &mut hidden);
                    cx.background_executor().timer(WATCH).await;
                    stop.store(true, std::sync::atomic::Ordering::Relaxed);
                    if let Ok(seen) = watcher.join() {
                        captures += seen.0;
                        bad += seen.1;
                        darker += seen.2;
                        lighter += seen.3;
                        worst = worst.max(seen.4);
                        spent += seen.5;
                        bad_swaps += usize::from(seen.1 > 0);
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(50))
                        .await;
                }
                let _ = writeln!(
                    report,
                    "{name}: {bad_swaps}/{SWAPS} swaps showed a change; {bad}/{captures} captures \
                     differ ({darker} darker = both, {lighter} lighter = neither), worst {worst} px; \
                     {:.1} ms per capture",
                    spent.as_secs_f64() * 1000.0 / f64::from(captures.max(1))
                );
            }
        } else {
            let _ = writeln!(report, "\nSWAP: the screen could not be captured");
        }

        // The bends, in whichever window shows now.
        let front = if shown == ha { a } else { b };
        front
            .update(cx, |view, _, cx| {
                view.bends = Some(Bends {
                    raster: Vec::new(),
                    at: Vec::new(),
                    image: None,
                    old: None,
                });
                cx.notify();
            })
            .ok();
        let started = Instant::now();
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let done = front
                .update(cx, |view, _, _| view.bends_done())
                .unwrap_or(true);
            if done || started.elapsed() > Duration::from_secs(20) {
                break;
            }
        }
        cx.background_executor().timer(Duration::from_millis(200)).await;
        let stats = front
            .update(cx, |view, _, _| {
                view.bends.as_ref().map(|b| {
                    let ms = |d: &Duration| d.as_secs_f64() * 1000.0;
                    let mut raster: Vec<f64> = b.raster.iter().map(ms).collect();
                    let mut gaps: Vec<f64> = b
                        .at
                        .windows(2)
                        .map(|w| ms(&w[1].duration_since(w[0])))
                        .collect();
                    raster.sort_by(f64::total_cmp);
                    gaps.sort_by(f64::total_cmp);
                    let pick = |v: &[f64], q: f64| {
                        v.get(((v.len() as f64 - 1.0) * q).round() as usize)
                            .copied()
                            .unwrap_or(0.0)
                    };
                    format!(
                        "{} frames of {BENDS} curves\ndrawing: median {:.2} ms, 95% {:.2} ms, worst {:.2} ms\n\
                         frame to frame: median {:.2} ms, 95% {:.2} ms, worst {:.2} ms",
                        b.at.len(),
                        pick(&raster, 0.5),
                        pick(&raster, 0.95),
                        pick(&raster, 1.0),
                        pick(&gaps, 0.5),
                        pick(&gaps, 0.95),
                        pick(&gaps, 1.0),
                    )
                })
            })
            .ok()
            .flatten();
        let _ = writeln!(
            report,
            "\nBENDS: over a {}x{} px window, only the bends drawn afresh each frame, packed in one small picture, the last one dropped\n{}",
            rect.width(),
            rect.height(),
            stats.unwrap_or_else(|| "did not run".into())
        );
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let _ = std::fs::create_dir_all(&folder);
        // The last frame as the screen shows it: whole and uncorrupted?
        if let Some((w, h, pixels)) = win::capture(rect) {
            let _ = image::save_buffer(
                folder.join(format!("probe-{stamp}-bends.png")),
                &pixels,
                w,
                h,
                image::ExtendedColorType::Rgba8,
            );
        }
        let _ = std::fs::write(folder.join(format!("probe-{stamp}.txt")), report);
        cx.update(|cx| {
            crate::view::remove(a.into(), cx);
            crate::view::remove(b.into(), cx);
        })
        .ok();
        RUNNING.set(false);
    })
    .detach();
}
