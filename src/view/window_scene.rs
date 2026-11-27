//! The big window, drawn from the scene.
//!
//! `scene::window` says where each part goes, in whole device px from the
//! window's corner; this paints them there. gitstat draws the title bar
//! itself: its colour, icon, name and three buttons, which Windows still
//! works (dragging, snapping, minimising, closing) through the places
//! this marks for it. What it paints itself (the title bar, the top
//! bar's words and bar, the columns' headers and lines) it first lists as
//! a picture (`scene::parts`) and then paints from that list: the list
//! the flip carries to the panel's. A column's tree and rows, a spine's
//! words, the pull requests and the bottom bar are ordinary GPUI
//! elements, the slots, laid out at the device px the layout gives them
//! and painted between the picture's fills and its words.

use super::panel_scene::{centred, count_words, hit, measure, put, width};
use super::{
    Cell, Mode, Palette, StatusView, cell_of, filter_key, prs_block, refresh, repo_row, rim,
    rim_width, section_tint, spine, status_color, table_cells, title_bar, titles, tone_counts,
};
use crate::config::Section;
use crate::git::RepoStatus;
use crate::layout::Rect;
use crate::scene::paint;
use crate::scene::panel::{HEADLINE as GLYPH, SMALL, TINY};
use crate::scene::parts::{Key, Part, Picture, Sheet, What};
use crate::scene::text::{Look, text};
use crate::scene::window::{
    self as place, CellSpec, HEAD_ICON, HEAD_LINE, HEAD_NAME, HEADLINE, HeadSpec,
};
use crate::state::AppState;
use crate::timeline::Timelines;
use gpui::{
    AnyElement, App, Bounds, ContentMask, Context, DispatchPhase, Element, ElementId,
    GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId,
    MouseExitEvent, MouseMoveEvent, ParentElement, Pixels, ScrollHandle, ScrollWheelEvent,
    SharedString, Style, Styled, Window, WindowAppearance, WindowControlArea, div, point,
    prelude::*, px, relative,
};
use std::path::PathBuf;
use std::sync::Arc;

/// The title bar's glyphs' size, logical px, as Windows 11 draws them.
const CAPTION_GLYPH: f32 = 10.0;
/// The top of the title bar left to Windows, logical px: where the mouse
/// sizes the window by its top edge.
const TOP_GRIP: f32 = 4.0;
/// The close button under the mouse.
const CLOSE_HOVER: u32 = 0xc42b1c;
/// The radius Windows 11 rounds a window's corners with, logical px.
const RADIUS: f32 = 8.0;

/// The font the title bar's glyphs come from: Windows 11's, else Windows
/// 10's.
fn caption_font(cx: &App) -> &'static str {
    static FONT: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    FONT.get_or_init(|| {
        let fluent = "Segoe Fluent Icons";
        if cx
            .text_system()
            .all_font_names()
            .iter()
            .any(|n| n == fluent)
        {
            fluent
        } else {
            "Segoe MDL2 Assets"
        }
    })
}

/// A cell of the table: a section's spine or a repo's column, with what
/// it shows when it is in sight.
enum CellPart {
    Spine(Section, Option<AnyElement>),
    Column(Option<Box<ColumnPart>>),
}

/// A repo's column: what its header says, and its slots.
struct ColumnPart {
    repo: Arc<RepoStatus>,
    color: Hsla,
    checkout: SharedString,
    status: SharedString,
    /// Where its rows' lines go, when it draws them.
    lines: Option<f32>,
    title: Option<AnyElement>,
    glance: Option<AnyElement>,
    body: Option<AnyElement>,
}

/// The big window as one element, as big as its window; or, in a flip,
/// drawn in the flip's window where its own window is on the screen.
pub(super) struct WindowScene {
    p: Palette,
    dark: bool,
    maximized: bool,
    font: &'static str,
    headline: SharedString,
    headline_color: Hsla,
    subline: SharedString,
    /// The health bar's counts, and whether a check runs.
    health: Option<([usize; 3], bool)>,
    settings: Option<AnyElement>,
    refresh: Option<AnyElement>,
    refresh_words: SharedString,
    checking: bool,
    cells: Vec<CellPart>,
    /// What shows in the table's place when there is no table.
    body: Option<AnyElement>,
    prs: Option<AnyElement>,
    footer: Option<AnyElement>,
    scroll: ScrollHandle,
    /// In a flip: its window's inside in the window that draws the flip,
    /// device px.
    stage: Option<Rect>,
}

/// The window laid out for this frame.
pub(super) struct Laid {
    layout: place::Layout,
    /// Its corner in the window that draws it, device px.
    at: (i32, i32),
    counts: Vec<(SharedString, Hsla)>,
    /// What each shown column says at a glance, the pull requests and
    /// the bottom bar, measured, yet to be put where they go.
    glances: Vec<Option<AnyElement>>,
    prs: Option<AnyElement>,
    footer: Option<AnyElement>,
}

/// The window's slots, laid out where they go.
pub(super) struct Slots {
    top: Vec<AnyElement>,
    cells: Vec<AnyElement>,
    prs: Option<AnyElement>,
    footer: Option<AnyElement>,
    table: Bounds<Pixels>,
}

/// What of the window takes the mouse: all of it, for the wheel, and the
/// title bar with its buttons.
pub(super) struct Hits {
    whole: Hitbox,
    drag: Hitbox,
    buttons: [Hitbox; 3],
}

impl WindowScene {
    /// Lays the window out: where each of its parts goes. Its slots are
    /// to be painted at `alpha`.
    pub(super) fn lay(
        &mut self,
        bounds: Bounds<Pixels>,
        alpha: f32,
        window: &mut Window,
        cx: &mut App,
    ) -> Laid {
        let s = window.scale_factor();
        let dev = |v: Pixels| (f32::from(v) * s).round() as i32;
        let (at, (w, h)) = match self.stage {
            Some(r) => ((r.left, r.top), (r.width(), r.height())),
            None => ((0, 0), (dev(bounds.size.width), dev(bounds.size.height))),
        };
        let counts = match self.health {
            Some((n, _)) => count_words(n, &self.p),
            None => Vec::new(),
        };
        let health = (counts.len() == 3).then(|| {
            let cw = |i: usize| width(&counts[i].0, TINY, window);
            [cw(0), cw(1), cw(2)]
        });
        let prs = self
            .prs
            .take()
            .map(|slot| measure(slot, w, s, alpha, window, cx));
        let footer = self
            .footer
            .take()
            .map(|slot| measure(slot, w, s, alpha, window, cx));
        // Each column's header, as tall as what it says at a glance.
        let inner = place::column(
            Rect::new(0, 0, place::px(place::COLUMN, s), i32::MAX / 2),
            &HeadSpec::default(),
            s,
        )
        .title
        .width();
        let mut glances = Vec::new();
        let specs: Vec<CellSpec> = self
            .cells
            .iter_mut()
            .map(|cell| match cell {
                CellPart::Spine(..) => CellSpec::Spine,
                CellPart::Column(None) => CellSpec::Column(HeadSpec::default()),
                CellPart::Column(Some(part)) => {
                    let glance = part
                        .glance
                        .take()
                        .map(|slot| measure(slot, inner, s, alpha, window, cx));
                    let tall = glance.as_ref().map(|(_, tall)| *tall);
                    glances.push(glance.map(|(el, _)| el));
                    crate::repo_marks::want(&part.repo, HEAD_ICON, window, cx);
                    CellSpec::Column(HeadSpec {
                        checkout: width(&part.checkout, HEAD_LINE, window),
                        status: width(&part.status, HEAD_LINE, window),
                        glance: tall,
                    })
                }
            })
            .collect();
        let wanted = dev(-self.scroll.offset().x);
        let layout = place::layout(&place::Spec {
            scale: s,
            size: (w, h),
            health,
            refresh: width(&self.refresh_words, SMALL, window),
            cells: specs,
            scroll: wanted,
            prs: prs.as_ref().map(|(_, tall)| *tall),
            footer: footer.as_ref().map_or(0, |(_, tall)| *tall),
        });
        // Scrolled past its end (the window grew, a section folded): back
        // to it. The window's own doing: a flip only draws it.
        if layout.scroll != wanted && self.stage.is_none() {
            self.scroll
                .set_offset(point(px(-(layout.scroll as f32) / s), px(0.0)));
        }
        Laid {
            layout,
            at,
            counts,
            glances,
            prs: prs.map(|(el, _)| el),
            footer: footer.map(|(el, _)| el),
        }
    }

    /// Everything the window paints itself, device px from the corner of
    /// the window that draws it, in the order it is painted. In a flip,
    /// with the sheet, rim and shadow Windows gives its own window.
    /// `hovered` is the title bar button under the mouse.
    pub(super) fn picture(&self, laid: &Laid, s: f32, hovered: Option<usize>) -> Picture {
        let p = self.p;
        let l = &laid.layout;
        let at = laid.at;
        let sh = |r: Rect| Rect::new(r.left + at.0, r.top + at.1, r.right + at.0, r.bottom + at.1);
        let own = Key::Own;
        let mut parts = Vec::new();
        let whole = Rect::new(0, 0, l.size.0, l.size.1);

        // Its ground: its own in its window, the sheet's in a flip.
        let staged = self.stage.is_some();
        let shaped = staged && !self.maximized;
        let rim_px = if shaped {
            rim_width(s).round() as i32
        } else {
            0
        };
        let radius = if shaped { RADIUS } else { 0.0 };
        let inner = (radius - rim_px as f32 / s).max(0.0);
        if !staged {
            parts.push(Part::fill(own.clone(), whole, p.bg));
        }

        // The title bar.
        let cap = &l.caption;
        parts.push(
            Part::fill(Key::Caption, sh(cap.rect), title_bar(self.dark))
                .radii([inner, inner, 0.0, 0.0]),
        );
        let glyphs = [
            "\u{e921}",
            if self.maximized {
                "\u{e923}"
            } else {
                "\u{e922}"
            },
            "\u{e8bb}",
        ];
        let mut glyph_parts = Vec::new();
        for (i, (glyph, r)) in glyphs.into_iter().zip(cap.buttons).enumerate() {
            let mut color = p.text;
            if hovered == Some(i) {
                let wash: Hsla = match (i, self.dark) {
                    (2, _) => {
                        color = gpui::white();
                        gpui::rgb(CLOSE_HOVER).into()
                    }
                    (_, true) => gpui::white().opacity(0.06),
                    (_, false) => gpui::black().opacity(0.04),
                };
                parts.push(Part::fill(own.clone(), sh(r), wash));
            }
            glyph_parts.push(
                Part::words(
                    own.clone(),
                    sh(centred(r, CAPTION_GLYPH, s)),
                    glyph,
                    Look::new(CAPTION_GLYPH, color).family(self.font),
                )
                .centred(),
            );
        }
        parts.push(Part::new(own.clone(), sh(cap.icon), What::AppIcon));
        parts.push(Part::words(
            own.clone(),
            sh(cap.title),
            "gitstat",
            Look::new(TINY, p.text),
        ));
        parts.extend(glyph_parts);

        // The top bar.
        let h = &l.header;
        parts.push(Part::words(
            Key::Headline,
            sh(h.headline),
            self.headline.clone(),
            Look::new(HEADLINE, self.headline_color).bold(),
        ));
        parts.push(Part::words(
            Key::Subline,
            sh(h.subline),
            self.subline.clone(),
            Look::new(TINY, p.muted),
        ));
        if let (Some(bar), Some((counts, checking))) = (h.bar, self.health) {
            parts.push(Part::new(
                Key::Health,
                sh(bar),
                What::Health {
                    counts,
                    colors: [p.green, p.amber, p.red],
                    track: p.hover,
                    checking,
                },
            ));
            for (i, ((t, color), r)) in laid.counts.iter().zip(h.counts).enumerate() {
                parts.push(Part::words(
                    Key::Count(i as u8),
                    sh(r),
                    t.clone(),
                    Look::new(TINY, *color),
                ));
            }
        }
        let dim = |c: Hsla| if self.checking { c.opacity(0.6) } else { c };
        parts.push(Part::fill(own.clone(), sh(h.refresh), dim(p.button)).radii([6.0; 4]));
        parts.push(
            Part::words(
                Key::Settings,
                sh(centred(h.settings, GLYPH, s)),
                "⚙",
                Look::new(GLYPH, p.text),
            )
            .centred(),
        );
        parts.push(Part::words(
            own.clone(),
            sh(h.refresh_words),
            self.refresh_words.clone(),
            Look::new(SMALL, dim(p.button_text)),
        ));
        parts.push(Part::fill(Key::HeaderLine, sh(h.line), p.border));

        // The table.
        parts.push(Part::fill(Key::Body, sh(l.table), p.surface));
        let line = place::px(1.0, s).max(1);
        for ((cell, r), c) in self.cells.iter().zip(&l.cells).zip(&l.columns) {
            match (cell, c) {
                (CellPart::Spine(section, _), _) => {
                    parts.push(
                        Part::fill(Key::Tint(*section), sh(*r), section_tint(*section, &p))
                            .clipped(),
                    );
                    let edge = Rect::new(r.right - line, r.top, r.right, r.bottom);
                    parts.push(Part::fill(own.clone(), sh(edge), p.border).clipped());
                }
                (CellPart::Column(Some(part)), Some(c)) => {
                    let path = &part.repo.path;
                    if let Some(top) = part.lines {
                        let tall = c.body.height() as f32 / s;
                        for y in crate::timeline::row_lines(top, tall, s) {
                            let r = Rect::new(
                                c.body.left,
                                c.body.top + y,
                                c.body.right,
                                c.body.top + y + line,
                            );
                            parts.push(Part::fill(own.clone(), sh(r), p.border).clipped());
                        }
                    }
                    parts.push(Part::fill(own.clone(), sh(c.line), p.border).clipped());
                    parts.push(Part::fill(own.clone(), sh(c.edge), p.border).clipped());
                    parts.push(
                        Part::new(
                            Key::Icon(path.clone()),
                            sh(c.icon),
                            What::Icon {
                                status: part.repo.clone(),
                                side: HEAD_ICON,
                                dot: part.color,
                                ends: None,
                            },
                        )
                        .clipped(),
                    );
                    parts.push(
                        Part::words(
                            Key::Name(path.clone()),
                            sh(c.name),
                            part.repo.name.clone(),
                            Look::new(HEAD_NAME, p.text).bold(),
                        )
                        .clipped(),
                    );
                    parts.push(
                        Part::words(
                            Key::Checkout(path.clone()),
                            sh(c.checkout),
                            part.checkout.clone(),
                            Look::new(HEAD_LINE, p.text),
                        )
                        .clipped(),
                    );
                    parts.push(
                        Part::words(
                            Key::Status(path.clone()),
                            sh(c.status),
                            part.status.clone(),
                            Look::new(HEAD_LINE, part.color),
                        )
                        .clipped(),
                    );
                }
                _ => {}
            }
        }
        Picture {
            sheet: staged.then(|| Sheet {
                frame: sh(whole.outset(rim_px)),
                radius,
                rim: rim_px,
                rim_color: rim(self.dark),
                bg: p.bg,
                shadow: if shaped { 1.0 } else { 0.0 },
            }),
            body: sh(l.table),
            parts,
        }
    }

    /// Where each shown repo's tree is drawn: its column's body, device
    /// px from the corner of the window that draws it.
    pub(super) fn bodies(&self, laid: &Laid) -> Vec<(PathBuf, Rect)> {
        let at = laid.at;
        self.cells
            .iter()
            .zip(&laid.layout.columns)
            .filter_map(|(cell, c)| match (cell, c) {
                (CellPart::Column(Some(part)), Some(c)) => {
                    let r = c.body;
                    Some((
                        part.repo.path.clone(),
                        Rect::new(r.left + at.0, r.top + at.1, r.right + at.0, r.bottom + at.1),
                    ))
                }
                _ => None,
            })
            .collect()
    }

    /// Lays the slots out where they go, to be painted at `alpha` of
    /// their strength.
    pub(super) fn slots(
        &mut self,
        laid: &mut Laid,
        alpha: f32,
        window: &mut Window,
        cx: &mut App,
    ) -> Slots {
        let s = window.scale_factor();
        let (layout, at) = (&laid.layout, laid.at);
        // The top bar's.
        let mut top = Vec::new();
        if let Some(slot) = self.settings.take() {
            top.push(put(slot, layout.header.settings, at, s, alpha, window, cx));
        }
        if let Some(slot) = self.refresh.take() {
            top.push(put(slot, layout.header.refresh, at, s, alpha, window, cx));
        }

        // The table's, cut to the table; or what shows in its place.
        let table = paint::bounds(layout.table, at, s);
        let mut cells = Vec::new();
        let mut glances = std::mem::take(&mut laid.glances).into_iter();
        let (left, right) = (0, layout.size.0);
        window.with_content_mask(Some(ContentMask { bounds: table }), |window| {
            if let Some(slot) = self.body.take() {
                cells.push(put(slot, layout.table, at, s, alpha, window, cx));
                return;
            }
            for ((cell, r), column) in self
                .cells
                .iter_mut()
                .zip(&layout.cells)
                .zip(&layout.columns)
            {
                match (cell, column) {
                    (CellPart::Spine(_, slot), _) => {
                        if let Some(slot) = slot.take()
                            && r.right > left
                            && r.left < right
                        {
                            cells.push(put(slot, *r, at, s, alpha, window, cx));
                        }
                    }
                    (CellPart::Column(Some(part)), Some(c)) => {
                        if let Some(slot) = part.body.take() {
                            cells.push(put(slot, c.body, at, s, alpha, window, cx));
                        }
                        if let Some(slot) = part.title.take() {
                            cells.push(put(slot, c.title, at, s, alpha, window, cx));
                        }
                        if let (Some(mut el), Some(g)) = (glances.next().flatten(), c.glance) {
                            el.prepaint_at(paint::bounds(g, at, s).origin, window, cx);
                            cells.push(el);
                        }
                    }
                    _ => {}
                }
            }
        });
        let prs = laid.prs.take().zip(layout.prs).map(|(mut el, r)| {
            el.prepaint_at(paint::bounds(r, at, s).origin, window, cx);
            el
        });
        let footer = laid.footer.take().map(|mut el| {
            el.prepaint_at(paint::bounds(layout.footer, at, s).origin, window, cx);
            el
        });
        Slots {
            top,
            cells,
            prs,
            footer,
            table,
        }
    }

    pub(super) fn paint_slots(slots: &mut Slots, window: &mut Window, cx: &mut App) {
        window.with_content_mask(
            Some(ContentMask {
                bounds: slots.table,
            }),
            |window| {
                for slot in &mut slots.cells {
                    slot.paint(window, cx);
                }
            },
        );
        for slot in &mut slots.top {
            slot.paint(window, cx);
        }
        if let Some(prs) = &mut slots.prs {
            prs.paint(window, cx);
        }
        if let Some(footer) = &mut slots.footer {
            footer.paint(window, cx);
        }
    }
}

impl IntoElement for WindowScene {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for WindowScene {
    type RequestLayoutState = ();
    type PrepaintState = Option<(Laid, Slots, Hits)>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let s = window.scale_factor();
        let mut laid = self.lay(bounds, 1.0, window, cx);
        // The whole window takes the wheel; over it, the title bar: the
        // bar drags the window, its buttons are Windows' own three.
        let whole = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let grip = if self.maximized {
            0
        } else {
            place::px(TOP_GRIP, s)
        };
        let under_grip =
            |r: Rect| Rect::new(r.left, r.top + grip, r.right, r.bottom.max(r.top + grip));
        let cap = &laid.layout.caption;
        let drag = window.insert_hitbox(
            paint::bounds(under_grip(cap.rect), laid.at, s),
            HitboxBehavior::Normal,
        );
        let buttons = cap.buttons.map(|r| {
            window.insert_hitbox(
                paint::bounds(under_grip(r), laid.at, s),
                HitboxBehavior::Normal,
            )
        });
        let slots = self.slots(&mut laid, 1.0, window, cx);
        Some((
            laid,
            slots,
            Hits {
                whole,
                drag,
                buttons,
            },
        ))
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        state: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some((laid, slots, hits)) = state else {
            return;
        };
        let s = window.scale_factor();
        let l = &laid.layout;
        let hovered = hits.buttons.iter().position(|h| h.is_hovered(window));
        let picture = self.picture(laid, s, hovered);
        // The ground and the fills; over them the slots; over those the
        // words and pictures.
        picture.paint_under(window, cx);

        // The title bar. Windows asks which of its parts the mouse is on:
        // the three buttons first, then the bar that drags the window.
        for (area, hitbox) in [
            WindowControlArea::Min,
            WindowControlArea::Max,
            WindowControlArea::Close,
        ]
        .into_iter()
        .zip(&hits.buttons)
        {
            window.insert_window_control_hitbox(area, hitbox.clone());
        }
        window.insert_window_control_hitbox(WindowControlArea::Drag, hits.drag.clone());
        for (i, hitbox) in hits.buttons.iter().enumerate() {
            let was = hovered == Some(i);
            // Drawn again when the mouse comes onto it or leaves it.
            let over = hitbox.clone();
            window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, _| {
                if phase == DispatchPhase::Capture && over.is_hovered(window) != was {
                    window.refresh();
                }
            });
            // GPUI keeps the mouse where it last was when it leaves the
            // window, and the button lit: tell it where the mouse is now.
            let over = hitbox.clone();
            window.on_mouse_event(move |_: &MouseExitEvent, _, window, _| {
                if over.is_hovered(window)
                    && let Some(h) = crate::win::hwnd(window)
                    && let Some(frame) = crate::win::window_rect(h)
                {
                    // Only once it is off the window altogether: over
                    // the button it has left the window's inside alone.
                    let (x, y) = crate::win::cursor_pos();
                    if !(frame.left..frame.right).contains(&x)
                        || !(frame.top..frame.bottom).contains(&y)
                    {
                        crate::win::tell_mouse(h, x, y);
                    }
                }
            });
        }

        // The wheel anywhere scrolls the table sideways, unless what is
        // under the mouse takes it first (a column's commits, the pull
        // requests): registered before the slots paint, it comes last.
        let (scroll, whole, max) = (
            self.scroll.clone(),
            hits.whole.clone(),
            px(l.max_scroll as f32 / s),
        );
        let prs_bounds = l.prs.map(|r| paint::bounds(r, laid.at, s));
        window.on_mouse_event(move |e: &ScrollWheelEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble
                || !whole.should_handle_scroll(window)
                || prs_bounds.is_some_and(|r| r.contains(&e.position))
            {
                return;
            }
            let d = e.delta.pixel_delta(window.line_height());
            let dx = if d.x.abs() > d.y.abs() { d.x } else { d.y };
            let was = scroll.offset();
            let x = (was.x + dx).clamp(-max, px(0.0));
            if x != was.x {
                scroll.set_offset(point(x, px(0.0)));
                window.refresh();
                cx.stop_propagation();
            }
        });
        Self::paint_slots(slots, window, cx);
        picture.paint_over(window, cx);
        crate::flip::painted();
        crate::flip::window_drew();
    }
}

/// The window's bottom bar: what a click and the wheel do, or what the
/// repo list left out, and the way to the settings file.
fn footer(state: &AppState, p: &Palette) -> AnyElement {
    let config_file = state.config_file.clone();
    let text_color = p.text;
    let capped = state
        .capped
        .as_ref()
        .map(|c| format!("Repo list incomplete: {c}"));
    let amber = capped.is_some();
    let words = capped.unwrap_or_else(|| {
        match state.config.repo_click {
            crate::config::RepoClick::Folder => {
                "Click a repo's name to open its folder. Wheel over the commits scrolls them, \
                 anywhere else the table."
            }
            crate::config::RepoClick::Github => {
                "Click a repo's name to open it on GitHub. Wheel over the commits scrolls them, \
                 anywhere else the table."
            }
        }
        .into()
    });
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_4()
        .py_2()
        .border_t_1()
        .border_color(p.border)
        .text_xs()
        .text_color(p.muted)
        .child(
            div()
                .min_w_0()
                .truncate()
                .when(amber, |d| d.text_color(p.amber))
                .child(text(words)),
        )
        .child(
            div()
                .id("config")
                .flex_none()
                .cursor_pointer()
                .hover(move |s| s.text_color(text_color))
                .child(text("Edit config.json"))
                .on_click(move |_, _, cx| cx.open_with_system(&config_file)),
        )
        .into_any_element()
}

impl StatusView {
    /// The big window as the scene draws it: in its own window, or, for
    /// a flip, `staged` in the panel's, where its window is (device px in
    /// that window) and whether it is maximised.
    pub(super) fn window_scene(
        &mut self,
        staged: Option<(Rect, bool)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> WindowScene {
        let p = Palette::for_appearance(window.appearance());
        let dark = matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let s = window.scale_factor();
        let timelines = self.timelines.clone();
        // A strip's label asked for its repo's column: the window's own
        // to bring into sight, as it first draws.
        let focus = self
            .timelines
            .as_ref()
            .filter(|_| staged.is_none())
            .and_then(|t| t.update(cx, |t, _| t.take_focus()));
        // A flip draws the table where the window itself has it.
        let table = match staged {
            Some(_) => super::SCROLLS
                .with(|s| s.borrow().get(false))
                .unwrap_or_default(),
            None => self.table.clone(),
        };
        let state = self.state.clone();
        let state = state.read(cx);
        let sweeping = state.sweeping;
        let (headline, headline_color, subline) = titles(state, &p);
        let health = (state.has_result && !state.statuses.is_empty())
            .then(|| (tone_counts(&state.statuses), sweeping));

        let live = staged.is_none();
        let settings = live.then(|| {
            hit("settings", true, &p)
                .rounded_md()
                .on_click(|_, _, cx| crate::settings::show(cx))
                .into_any_element()
        });
        let lighter = gpui::white().opacity(0.1);
        let refresh_slot = live.then(|| {
            div()
                .id("refresh")
                .size_full()
                .rounded_md()
                .when(!sweeping, |d| {
                    d.cursor_pointer().hover(move |st| st.bg(lighter))
                })
                .on_click(move |_, _, cx| {
                    if !sweeping {
                        refresh(cx);
                    }
                })
                .into_any_element()
        });

        // Each section's spine, then its repos' columns unless folded;
        // the GitHub repos with no clone come last. Only the cells in
        // sight are built: scrolling draws the view again.
        let no_local = state.has_result && state.statuses.is_empty() && state.problem().is_none();
        let empty = no_local && state.remote_statuses.is_empty();
        let mut cells = Vec::new();
        let mut body = None;
        match &timelines {
            Some(t) if !empty => {
                let all = table_cells(state);
                let kinds: Vec<CellSpec> = all
                    .iter()
                    .map(|c| match c {
                        Cell::Spine(..) => CellSpec::Spine,
                        Cell::Repo(_) => CellSpec::Column(HeadSpec::default()),
                    })
                    .collect();
                let edges = place::cell_edges(&kinds, s);
                let view = match staged {
                    Some((r, _)) => r.width(),
                    None => (f32::from(window.viewport_size().width) * s).round() as i32,
                };
                let max = (edges.last().copied().unwrap_or(0) - view).max(0);
                let mut scroll = (-f32::from(table.offset().x) * s).round() as i32;
                // The asked-for column, wholly in sight.
                if let Some(i) = focus.as_deref().and_then(|f| cell_of(&all, f)) {
                    scroll = scroll.max(edges[i + 1] - view).min(edges[i]);
                }
                let scroll = scroll.clamp(0, max);
                if live {
                    table.set_offset(point(px(-(scroll as f32) / s), px(0.0)));
                }
                for (i, cell) in all.iter().enumerate() {
                    let seen = edges[i + 1] > scroll && edges[i] < scroll + view;
                    cells.push(match *cell {
                        Cell::Spine(section, n) => CellPart::Spine(
                            section,
                            seen.then(|| spine(section, n, state.folded(section), &p)),
                        ),
                        Cell::Repo(repo) => CellPart::Column(seen.then(|| {
                            let c = Timelines::column(t, i, repo, &p, dark, live, window, cx);
                            Box::new(ColumnPart {
                                repo: Arc::new(repo.clone()),
                                color: status_color(repo, &p),
                                checkout: c.checkout,
                                status: c.status,
                                lines: c.lines,
                                title: live.then_some(c.title),
                                glance: c.glance,
                                body: Some(c.body),
                            })
                        })),
                    });
                }
            }
            _ => {
                // Without timelines (never, once the app is up) a plain
                // list; with no repo at all, why.
                let rows: Vec<AnyElement> = if timelines.is_none() {
                    state
                        .statuses
                        .iter()
                        .enumerate()
                        .map(|(i, st)| repo_row(i, st, &p, Mode::Window, cx))
                        .collect()
                } else {
                    Vec::new()
                };
                let why = match state.config.depth() {
                    1 => "No git repos directly inside this folder. Set \"path\" or \"depth\" in \
                          config.json."
                        .to_string(),
                    depth => format!(
                        "No git repos in this folder, down to {depth} levels below it. Set \
                         \"path\" or \"depth\" in config.json."
                    ),
                };
                body = Some(
                    div()
                        .id("repos")
                        .flex()
                        .flex_col()
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.list)
                        .children(rows)
                        .when(no_local, |list| {
                            list.child(div().p_4().text_sm().text_color(p.muted).child(text(why)))
                        })
                        .into_any_element(),
                );
            }
        }

        WindowScene {
            p,
            dark,
            maximized: match staged {
                Some((_, maximized)) => maximized,
                None => window.is_maximized(),
            },
            font: caption_font(cx),
            headline,
            headline_color,
            subline: subline.into(),
            health,
            settings,
            refresh: refresh_slot,
            refresh_words: if sweeping {
                "Checking…"
            } else {
                "Refresh now"
            }
            .into(),
            checking: sweeping,
            cells,
            body,
            prs: prs_block(state, Mode::Window, false, &p, cx),
            footer: Some(footer(state, &p)),
            scroll: table,
            stage: staged.map(|(r, _)| r),
        }
    }

    /// The big window: one element that draws it, under the root that
    /// takes its keys.
    pub(super) fn render_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = Palette::for_appearance(window.appearance());
        let dark = matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let filter_state = self.state.clone();
        let scene = self.window_scene(None, window, cx);
        div()
            .id("root")
            // Digits of one width, so counts and ⬆⬇ numbers line up.
            .map(|mut d| {
                d.text_style()
                    .get_or_insert_with(Default::default)
                    .font_features = Some(gpui::FontFeatures(std::sync::Arc::new(vec![(
                    "tnum".to_string(),
                    1,
                )])));
                d
            })
            .track_focus(&self.focus)
            .key_context("StatusView")
            .on_key_down(move |event, window, cx| {
                if filter_key(&filter_state, event, Mode::Window, window, cx) {
                    cx.stop_propagation();
                    return;
                }
                if event.keystroke.key == "escape" && !crate::flip::flip_back(window, cx) {
                    window.remove_window();
                }
            })
            .size_full()
            .text_color(p.text)
            .child(scene)
            .children(
                self.timelines
                    .as_ref()
                    .and_then(|t| t.read(cx).hover_card(&p, dark, cx)),
            )
            .into_any_element()
    }
}
