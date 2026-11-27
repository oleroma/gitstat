//! The painters of the parts that aren't words: filled and rounded
//! boxes and the health bar, on the whole device px their layout gives.

use crate::layout::Rect;
use gpui::{Bounds, Hsla, Pixels, Window, point, px, size};

/// `r`, device px from a view's corner, as GPUI's bounds in a window
/// whose scale is `s`, the view's corner at device px `at`.
pub fn bounds(r: Rect, at: (i32, i32), s: f32) -> Bounds<Pixels> {
    Bounds::new(
        point(
            px((r.left + at.0) as f32 / s),
            px((r.top + at.1) as f32 / s),
        ),
        size(
            px(r.width().max(0) as f32 / s),
            px(r.height().max(0) as f32 / s),
        ),
    )
}

/// Fills `b` with `color`.
pub fn fill(window: &mut Window, b: Bounds<Pixels>, color: Hsla) {
    if color.a > 0.0 && b.size.width > px(0.0) && b.size.height > px(0.0) {
        window.paint_quad(gpui::fill(b, color));
    }
}

/// Fills `b` with `color`, its corners rounded by `radii`, logical px.
pub fn round(window: &mut Window, b: Bounds<Pixels>, radii: gpui::Corners<Pixels>, color: Hsla) {
    if color.a > 0.0 && b.size.width > px(0.0) && b.size.height > px(0.0) {
        window.paint_quad(gpui::fill(b, color).corner_radii(radii));
    }
}

/// The health bar in `bar` (device px from the corner at `at`): a track,
/// and over it the repos' tones, each as wide as its share of `counts`,
/// their edges on whole device px. Dimmed while a check runs.
pub fn health(
    window: &mut Window,
    bar: Rect,
    at: (i32, i32),
    counts: [usize; 3],
    colors: [Hsla; 3],
    track: Hsla,
    checking: bool,
) {
    if bar.width() <= 0 || bar.height() <= 0 {
        return;
    }
    let s = window.scale_factor();
    let dim = |c: Hsla| if checking { c.opacity(0.6) } else { c };
    let all = bounds(bar, at, s);
    round(
        window,
        all,
        gpui::Corners::all(all.size.height / 2.0),
        dim(track),
    );
    let total: usize = counts.iter().sum();
    if total == 0 {
        return;
    }
    let edge = |n: usize| bar.left + (bar.width() as f32 * n as f32 / total as f32).round() as i32;
    let mut done = 0;
    for (n, color) in counts.into_iter().zip(colors) {
        let part = Rect::new(edge(done), bar.top, edge(done + n), bar.bottom);
        done += n;
        fill(window, bounds(part, at, s), dim(color));
    }
}

/// gitstat's own icon, as the exe carries it.
const APP_ICON: &str = include_str!("../../assets/gitstat.svg");

thread_local! {
    /// The app's icon, by its side in device px.
    static APP_ICONS: std::cell::RefCell<std::collections::HashMap<u32, Option<std::sync::Arc<gpui::RenderImage>>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

thread_local! {
    /// Pictures at part strength, by the picture and the strength in
    /// 32nds: made as a part fades, let go when the fading is over.
    static FADED: std::cell::RefCell<
        std::collections::HashMap<(usize, u8), std::sync::Arc<gpui::RenderImage>>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Paints `image` in `b` at `alpha` of its strength. GPUI paints a picture
/// whole or not at all, so one part way is a copy made at that strength.
pub fn image(
    window: &mut Window,
    b: Bounds<Pixels>,
    image: std::sync::Arc<gpui::RenderImage>,
    alpha: f32,
) {
    let step = (alpha.clamp(0.0, 1.0) * 32.0).round() as u8;
    if step == 0 {
        return;
    }
    let shown = if step >= 32 {
        Some(image)
    } else {
        let key = (std::sync::Arc::as_ptr(&image) as usize, step);
        FADED.with(|faded| {
            if let Some(kept) = faded.borrow().get(&key) {
                return Some(kept.clone());
            }
            let size = image.size(0);
            let (w, h) = (size.width.0 as u32, size.height.0 as u32);
            let mut data = image.as_bytes(0)?.to_vec();
            for px in data.as_chunks_mut::<4>().0 {
                px[3] = (u16::from(px[3]) * u16::from(step) / 32) as u8;
            }
            let buffer = image::RgbaImage::from_raw(w, h, data)?;
            let made = std::sync::Arc::new(gpui::RenderImage::new([image::Frame::new(buffer)]));
            faded.borrow_mut().insert(key, made.clone());
            Some(made)
        })
    };
    if let Some(shown) = shown {
        window
            .paint_image(b, Default::default(), shown, 0, false)
            .ok();
    }
}

/// The fading is over: lets go of the pictures made for it.
pub fn forget_faded(window: &mut Window) {
    let faded = FADED.with(|faded| std::mem::take(&mut *faded.borrow_mut()));
    for (_, picture) in faded {
        window.drop_image(picture).ok();
    }
}

/// Paints the app's icon in the square `r` (device px from the corner at
/// `at`), drawn at exactly that many px, at `alpha` of its strength.
pub fn app_icon(window: &mut Window, r: Rect, at: (i32, i32), alpha: f32) {
    let n = r.width().min(r.height());
    if n <= 0 {
        return;
    }
    let n = n as u32;
    let image = APP_ICONS.with(|icons| {
        icons
            .borrow_mut()
            .entry(n)
            .or_insert_with(|| {
                use resvg::{tiny_skia, usvg};
                let tree = usvg::Tree::from_str(APP_ICON, &usvg::Options::default()).ok()?;
                let mut pixmap = tiny_skia::Pixmap::new(n, n)?;
                let k = n as f32 / tree.size().width().max(1.0);
                resvg::render(
                    &tree,
                    tiny_skia::Transform::from_scale(k, k),
                    &mut pixmap.as_mut(),
                );
                // GPUI wants BGRA.
                let mut rgba = pixmap.take_demultiplied();
                for px in rgba.as_chunks_mut::<4>().0 {
                    px.swap(0, 2);
                }
                let buffer = image::RgbaImage::from_raw(n, n, rgba)?;
                Some(std::sync::Arc::new(gpui::RenderImage::new([
                    image::Frame::new(buffer),
                ])))
            })
            .clone()
    });
    if let Some(picture) = image {
        let s = window.scale_factor();
        self::image(window, bounds(r, at, s), picture, alpha);
    }
}
