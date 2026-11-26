//! Each repo's own icon, `.gitstat/icon.svg`, in the app's windows: beside
//! its name in the panel and the window, and on its own in icons-only mode.
//!
//! Drawn by `repo_icon`'s renderer, with the same limits as the folder
//! icons, in the repo's status tone, at the exact device px it is shown at
//! and painted on whole device px, so it is never resampled. Drawing runs
//! off the UI thread; until it lands, or for a repo without an icon, a
//! status dot stands in. The file is read again whenever the repo is
//! checked, and drawn again only when it changed.

use crate::git::RepoStatus;
use crate::icon::{FolderState, Tone};
use crate::repo_icon::{self, Renderer};
use crate::state::AppState;
use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Entity, Hsla, IntoElement, RenderImage, Styled,
    Window, canvas, fill, point, px, size,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// How long after a draw that timed out, or had to wait for other icons,
/// it is tried again.
const RETRY_AFTER: Duration = Duration::from_secs(2);

/// A picture size: the tone and the device px.
type Key = (Tone, u32);

#[derive(Default)]
struct Entry {
    /// The repo's check count the file was last read at.
    checks: Option<u64>,
    reading: bool,
    /// The file, `None` without one or when it can't be read.
    svg: Option<Arc<Vec<u8>>>,
    /// Bumped when `svg` changes, so a drawing of the old file is dropped.
    generation: u64,
    /// Drawn pictures; `None` where drawing failed.
    images: HashMap<Key, Option<Arc<RenderImage>>>,
    drawing: HashSet<Key>,
}

pub struct RepoMarks {
    state: Entity<AppState>,
    renderer: Arc<Renderer>,
    entries: HashMap<PathBuf, Entry>,
}

impl RepoMarks {
    pub fn new(state: &Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(state, |this: &mut Self, state, cx| {
            let listed: HashSet<PathBuf> = state
                .read(cx)
                .statuses
                .iter()
                .map(|s| s.path.clone())
                .collect();
            this.entries.retain(|p, _| listed.contains(p));
        })
        .detach();
        Self {
            state: state.clone(),
            renderer: Arc::new(Renderer::default()),
            entries: HashMap::new(),
        }
    }

    /// What to show for `path` at `key`.
    fn look(&self, path: &Path, key: Key) -> Look {
        let Some(entry) = self.entries.get(path).filter(|e| e.svg.is_some()) else {
            return Look::Dot;
        };
        match entry.images.get(&key) {
            Some(Some(image)) => Look::Image(image.clone()),
            Some(None) => Look::Dot,
            None => Look::Drawing,
        }
    }

    /// Reads `path`'s file if it was checked since, and draws `key` if it
    /// isn't drawn.
    fn want(&mut self, path: &Path, key: Key, cx: &mut Context<Self>) {
        let checks = self.state.read(cx).checks(path);
        let entry = self.entries.entry(path.to_path_buf()).or_default();
        if !entry.reading && entry.checks != Some(checks) {
            entry.checks = Some(checks);
            entry.reading = true;
            let repo = path.to_path_buf();
            let task = cx.background_spawn(async move { repo_icon::load(&repo) });
            let repo = path.to_path_buf();
            cx.spawn(async move |this, cx| {
                let svg = task.await.ok().flatten();
                this.update(cx, |this, cx| this.read_done(&repo, svg, cx))
                    .ok();
            })
            .detach();
        }
        let Some(svg) = entry.svg.clone() else { return };
        if entry.images.contains_key(&key) || !entry.drawing.insert(key) {
            return;
        }
        let generation = entry.generation;
        let renderer = self.renderer.clone();
        let name = path.to_string_lossy().into_owned();
        let (tone, n) = key;
        let task = cx.background_spawn(async move {
            match renderer.render_rgba(&name, &svg, tone, &[n]) {
                Ok(images) => Ok(images
                    .into_iter()
                    .next()
                    .and_then(|(n, rgba)| to_image(n, rgba))),
                // Timed out or waiting for other icons: worth another try.
                Err(e) if e.transient => Err(()),
                Err(_) => Ok(None),
            }
        });
        let repo = path.to_path_buf();
        let retry = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let image = match task.await {
                Ok(image) => image,
                Err(()) => {
                    // Still marked as drawing until then, so frames in
                    // between don't ask again; the renderer keeps a late
                    // result for the next try.
                    retry.timer(RETRY_AFTER).await;
                    this.update(cx, |this, cx| {
                        if let Some(entry) = this.entries.get_mut(&repo)
                            && entry.generation == generation
                        {
                            entry.drawing.remove(&key);
                            cx.notify();
                        }
                    })
                    .ok();
                    return;
                }
            };
            this.update(cx, |this, cx| {
                let Some(entry) = this.entries.get_mut(&repo) else {
                    return;
                };
                if entry.generation != generation {
                    return;
                }
                entry.drawing.remove(&key);
                entry.images.insert(key, image);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn read_done(&mut self, path: &Path, svg: Option<Vec<u8>>, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get_mut(path) else {
            return;
        };
        entry.reading = false;
        if entry.svg.as_deref() == svg.as_ref() {
            return;
        }
        entry.svg = svg.map(Arc::new);
        entry.generation += 1;
        entry.images.clear();
        entry.drawing.clear();
        cx.notify();
    }
}

enum Look {
    Image(Arc<RenderImage>),
    /// Its icon is being drawn: nothing yet, rather than a dot for a moment.
    Drawing,
    /// No icon, or one that can't be drawn.
    Dot,
}

/// Straight RGBA to the BGRA GPUI takes.
fn to_image(n: u32, mut rgba: Vec<u8>) -> Option<Arc<RenderImage>> {
    for px in rgba.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(n, n, rgba)?;
    Some(Arc::new(RenderImage::new([image::Frame::new(buffer)])))
}

/// `status`'s icon, `side` logical px square, in its status tone; a dot in
/// `dot` colour while there is none.
pub fn mark(marks: &Entity<RepoMarks>, status: &RepoStatus, side: f32, dot: Hsla) -> AnyElement {
    let (marks1, marks2) = (marks.clone(), marks.clone());
    let (status1, status2) = (status.clone(), status.clone());
    canvas(
        move |_, window, cx| want_in(&marks1, &status1, side, window, cx),
        move |bounds, _, window, cx| paint_in(&marks2, &status2, bounds, side, dot, window, cx),
    )
    .size(px(side))
    .flex_none()
    .into_any_element()
}

fn want_in(
    marks: &Entity<RepoMarks>,
    status: &RepoStatus,
    side: f32,
    window: &Window,
    cx: &mut App,
) {
    let key = (FolderState::of(status).tone(), device_px(side, window));
    marks.update(cx, |m, cx| m.want(&status.path, key, cx));
}

fn paint_in(
    marks: &Entity<RepoMarks>,
    status: &RepoStatus,
    bounds: Bounds<gpui::Pixels>,
    side: f32,
    dot: Hsla,
    window: &mut Window,
    cx: &App,
) {
    let key = (FolderState::of(status).tone(), device_px(side, window));
    match marks.read(cx).look(&status.path, key) {
        Look::Image(image) => {
            // On whole device px, at the picture's own size.
            let scale = window.scale_factor();
            let origin = point(
                px((f32::from(bounds.origin.x) * scale).round() / scale),
                px((f32::from(bounds.origin.y) * scale).round() / scale),
            );
            let n = key.1 as f32 / scale;
            let at = Bounds::new(origin, size(px(n), px(n)));
            window
                .paint_image(at, Default::default(), image, 0, false)
                .ok();
        }
        Look::Dot => paint_dot(bounds, side, dot, window),
        Look::Drawing => {}
    }
    if status.off_trunk() {
        paint_branch_badge(bounds, side, window);
    }
}

/// Asks for `status`'s icon `side` logical px square, for [`paint`] to
/// draw once it is there: as a view lays itself out.
pub fn want(status: &RepoStatus, side: f32, window: &Window, cx: &mut App) {
    if let Some(marks) = cx
        .try_global::<crate::view::Ui>()
        .map(|ui| ui.marks.clone())
    {
        want_in(&marks, status, side, window, cx);
    }
}

/// Paints `status`'s icon, as [`mark`] does, in the box `bounds`, `side`
/// logical px square.
pub fn paint(
    status: &RepoStatus,
    bounds: Bounds<gpui::Pixels>,
    side: f32,
    dot: Hsla,
    window: &mut Window,
    cx: &App,
) {
    if let Some(ui) = cx.try_global::<crate::view::Ui>() {
        paint_in(&ui.marks, status, bounds, side, dot, window, cx);
    }
}

/// The badge on a repo's icon while it is checked out off its default
/// branch: the app icon's branch, a white line with an amber branch off
/// it, on a dark disc.
const BRANCH_BADGE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16">
<circle cx="8" cy="8" r="7.6" fill="#1e2a38" stroke="#ffffff" stroke-opacity="0.35" stroke-width="0.8"/>
<g fill="none" stroke-width="1.7" stroke-linecap="round">
<path stroke="#ffffff" d="M5.6 4.2 L5.6 11.8"/>
<path stroke="#ffb74d" d="M5.6 10.2 C5.6 7.7 10.6 9 10.6 6.2"/>
</g>
<circle cx="5.6" cy="4.2" r="1.7" fill="#ffffff"/>
<circle cx="5.6" cy="11.8" r="1.7" fill="#ffffff"/>
<circle cx="10.6" cy="6.2" r="1.7" fill="#ffb74d"/>
</svg>"##;

thread_local! {
    /// The branch badge, by device px.
    static BADGES: std::cell::RefCell<HashMap<u32, Option<Arc<RenderImage>>>> =
        std::cell::RefCell::new(HashMap::new());
}

/// The branch badge at `n` device px square.
fn branch_badge(n: u32) -> Option<Arc<RenderImage>> {
    BADGES.with(|b| {
        b.borrow_mut()
            .entry(n)
            .or_insert_with(|| {
                use resvg::{tiny_skia, usvg};
                let tree = usvg::Tree::from_str(BRANCH_BADGE, &usvg::Options::default()).ok()?;
                let mut pixmap = tiny_skia::Pixmap::new(n, n)?;
                let k = n as f32 / 16.0;
                resvg::render(
                    &tree,
                    tiny_skia::Transform::from_scale(k, k),
                    &mut pixmap.as_mut(),
                );
                to_image(n, pixmap.take_demultiplied())
            })
            .clone()
    })
}

/// The branch badge over the top-right corner of an icon `side` logical px
/// square at `bounds`, on whole device px.
pub(crate) fn paint_branch_badge(bounds: Bounds<gpui::Pixels>, side: f32, window: &mut Window) {
    let scale = window.scale_factor();
    let n = (side * 0.62 * scale).round().max(6.0) as u32;
    let Some(image) = branch_badge(n) else {
        return;
    };
    let w = n as f32 / scale;
    let snap = |v: f32| (v * scale).round() / scale;
    let right = f32::from(bounds.origin.x) + side;
    let at = Bounds::new(
        point(
            px(snap(right - w * 0.8)),
            px(snap(f32::from(bounds.origin.y) - w * 0.2)),
        ),
        size(px(w), px(w)),
    );
    window
        .paint_image(at, Default::default(), image, 0, false)
        .ok();
}

/// `status`'s icon picture at the first of `sides` (device px) that is
/// drawn, for the flip.
pub fn picture(status: &RepoStatus, sides: &[u32], cx: &App) -> Option<Arc<RenderImage>> {
    let ui = cx.try_global::<crate::view::Ui>()?;
    let marks = ui.marks.read(cx);
    let tone = FolderState::of(status).tone();
    sides
        .iter()
        .find_map(|&n| match marks.look(&status.path, (tone, n)) {
            Look::Image(image) => Some(image),
            _ => None,
        })
}

/// Whether [`mark`] draws `status`'s icon as a plain dot in its status
/// colour (a repo with no picture) at any of `sides` device px.
pub fn shows_dot(status: &RepoStatus, sides: &[u32], cx: &App) -> bool {
    let Some(ui) = cx.try_global::<crate::view::Ui>() else {
        return false;
    };
    let marks = ui.marks.read(cx);
    let tone = FolderState::of(status).tone();
    sides
        .iter()
        .any(|&n| matches!(marks.look(&status.path, (tone, n)), Look::Dot))
}

/// The dot [`mark`] draws in a box `side` logical px across.
pub fn paint_dot(bounds: Bounds<gpui::Pixels>, side: f32, dot: Hsla, window: &mut Window) {
    let r = (side * 0.22).max(3.0);
    let c = bounds.center();
    window.paint_quad(
        fill(
            Bounds::new(
                point(c.x - px(r), c.y - px(r)),
                size(px(2.0 * r), px(2.0 * r)),
            ),
            dot,
        )
        .corner_radii(px(r)),
    );
}

fn device_px(side: f32, window: &Window) -> u32 {
    (side * window.scale_factor()).round().max(1.0) as u32
}

/// [`mark`] with the app's icons; an empty square before the app is up.
pub fn global_mark(status: &RepoStatus, side: f32, dot: Hsla, cx: &App) -> AnyElement {
    match cx.try_global::<crate::view::Ui>() {
        Some(ui) => mark(&ui.marks, status, side, dot),
        None => gpui::div().size(px(side)).flex_none().into_any_element(),
    }
}
