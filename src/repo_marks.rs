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
            renderer
                .render_rgba(&name, &svg, tone, &[n])
                .ok()
                .and_then(|images| images.into_iter().next())
                .and_then(|(n, rgba)| to_image(n, rgba))
        });
        let repo = path.to_path_buf();
        cx.spawn(async move |this, cx| {
            let image = task.await;
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
    let path = status.path.clone();
    let tone = FolderState::of(status).tone();
    let (marks1, marks2) = (marks.clone(), marks.clone());
    let path2 = path.clone();
    canvas(
        move |_, window, cx| {
            let key = (tone, device_px(side, window));
            marks1.update(cx, |m, cx| m.want(&path, key, cx));
        },
        move |bounds, _, window, cx| {
            let key = (tone, device_px(side, window));
            match marks2.read(cx).look(&path2, key) {
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
                Look::Dot => {
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
                Look::Drawing => {}
            }
        },
    )
    .size(px(side))
    .flex_none()
    .into_any_element()
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
