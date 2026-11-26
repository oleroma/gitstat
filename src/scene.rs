//! One scene for every view.
//!
//! A view is a layout: a pure function from what it shows, its size, its
//! scroll and the screen's scale to where each part goes, in whole device
//! px. The panel, the big window and the flip between them lay out and
//! paint the same parts with the same code, so a part looks the same
//! wherever it is drawn, to the pixel.
//!
//! - [`panel`]: where the panel's parts go.
//! - [`text`]: words on whole device px, the same in every window.
//! - [`paint`]: the other parts' painters.

pub mod panel;

#[cfg(windows)]
pub mod paint;
#[cfg(windows)]
pub mod text;

/// A length in logical px as whole device px at `scale`.
pub fn px(v: f32, scale: f32) -> i32 {
    (v * scale).round() as i32
}
