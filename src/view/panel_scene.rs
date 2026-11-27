//! The panel, drawn from the scene.
//!
//! `scene::panel` says where each part goes, in whole device px from the
//! panel's corner; this paints them there, wherever in its window the
//! panel is now (`panel::sheet`): at rest, or part way through its slide.
//! What it paints itself (its sheet, the words, icons, bars and washes)
//! it first lists as a picture (`scene::parts`) and then paints from that
//! list: the list the flip carries to the big window's. What only the
//! mouse or the list needs (a row's tree and where its label is clicked,
//! the buttons, the pull requests, the bottom bar) are ordinary GPUI
//! elements, the slots, laid out at the device px the layout gives them
//! and painted between the picture's fills and its words.

use super::{
    Palette, StatusView, filter_key, fold, mode_text, prs_block, refresh, remote_problem_row, rim,
    rim_width, section_color, section_text, section_tint, status_color, tip, title_bar, titles,
    tone_counts,
};
use crate::config::Section;
use crate::git::RepoStatus;
use crate::layout::Rect;
use crate::panel;
use crate::scene::paint;
use crate::scene::panel::{self as place, HEADLINE, MARK, SMALL, TINY, line_height};
use crate::scene::parts::{Key, Part, Picture, Sheet, What};
use crate::scene::text::{self, Look, text};
use crate::state::AppState;
use crate::timeline::Timelines;
use crate::win;
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, ContentMask, Context, DispatchPhase, Element,
    ElementId, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement,
    LayoutId, ParentElement, Pixels, ScrollHandle, ScrollWheelEvent, SharedString, Style, Styled,
    Window, WindowAppearance, div, point, prelude::*, px, relative, size,
};
use std::path::PathBuf;
use std::sync::Arc;

/// A button on the top bar.
struct Button {
    key: Key,
    glyph: SharedString,
    /// The glyph's font, when not the window's own.
    family: Option<&'static str>,
    enabled: bool,
    slot: Option<AnyElement>,
}

/// A section: its heading and, unless folded, its rows.
struct SectionPart {
    section: Section,
    count: usize,
    open: bool,
    color: Hsla,
    words: SharedString,
    slot: Option<AnyElement>,
    rows: Vec<RowPart>,
}

/// A repo's row.
struct RowPart {
    repo: Arc<RepoStatus>,
    color: Hsla,
    checkout: SharedString,
    status: SharedString,
    /// Its tree, and where the mouse finds its label.
    graph: Option<AnyElement>,
    label: Option<AnyElement>,
}

/// The panel as one element, as big as its window: it draws itself where
/// in that window it is to be.
pub(super) struct PanelScene {
    p: Palette,
    dark: bool,
    headline: SharedString,
    headline_color: Hsla,
    subline: SharedString,
    /// The health bar's counts, and whether a check runs.
    health: Option<([usize; 3], bool)>,
    buttons: Vec<Button>,
    sections: Vec<SectionPart>,
    tail: Vec<AnyElement>,
    footer: Option<AnyElement>,
    scroll: ScrollHandle,
}

/// The panel laid out for this frame.
pub(super) struct Laid {
    layout: place::Layout,
    /// The panel's corner in the window, device px.
    at: (i32, i32),
    /// The next frame differs: it is sliding.
    moving: bool,
    counts: Vec<(SharedString, Hsla)>,
    /// What follows the rows and the bottom bar, measured, yet to be put
    /// where they go.
    tail: Vec<AnyElement>,
    footer: Option<AnyElement>,
}

/// The panel's slots, laid out where they go: the top bar's, the list's
/// top to bottom, and the bottom bar.
pub(super) struct Slots {
    top: Vec<AnyElement>,
    listed: Vec<AnyElement>,
    footer: Option<AnyElement>,
    list: Hitbox,
    list_bounds: Bounds<Pixels>,
}

/// `slot` in a box of its own, `r` device px from the corner at `at`,
/// laid out and ready to paint, at `alpha` of its strength.
pub(crate) fn put(
    slot: AnyElement,
    r: Rect,
    at: (i32, i32),
    s: f32,
    alpha: f32,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let b = paint::bounds(r, at, s);
    let mut el = div()
        .flex()
        .flex_col()
        .w(b.size.width)
        .h(b.size.height)
        .when(alpha < 1.0, |d| d.opacity(alpha))
        .child(slot)
        .into_any_element();
    el.prepaint_as_root(
        paint::slot_origin(r, at, s),
        size(
            AvailableSpace::Definite(b.size.width),
            AvailableSpace::Definite(b.size.height),
        ),
        window,
        cx,
    );
    el
}

/// `slot` laid out `width` device px wide, and how tall it comes out; to
/// be painted at `alpha` of its strength.
pub(super) fn measure(
    slot: AnyElement,
    width: i32,
    s: f32,
    alpha: f32,
    window: &mut Window,
    cx: &mut App,
) -> (AnyElement, i32) {
    let w = px(width as f32 / s);
    let mut el = div()
        .flex()
        .flex_col()
        .w(w)
        .when(alpha < 1.0, |d| d.opacity(alpha))
        .child(slot)
        .into_any_element();
    let laid = el.layout_as_root(
        size(AvailableSpace::Definite(w), AvailableSpace::MinContent),
        window,
        cx,
    );
    (el, (f32::from(laid.height) * s).round() as i32)
}

/// A text's width, whole device px, never narrower than it runs.
pub(super) fn width(words: &str, size: f32, window: &Window) -> i32 {
    let look = Look::new(size, gpui::black());
    (text::width(words, &look, window) * window.scale_factor()).ceil() as i32
}

fn arrow(open: bool) -> &'static str {
    if open { "▾" } else { "▸" }
}

/// A glyph's line, centred on its button.
pub(super) fn centred(r: Rect, size: f32, s: f32) -> Rect {
    let lh = place::px(line_height(size), s);
    let top = r.top + (r.height() - lh) / 2;
    Rect::new(r.left, top, r.right, top + lh)
}

/// The health bar's counts in words, each in its colour.
pub(super) fn count_words(n: [usize; 3], p: &Palette) -> Vec<(SharedString, Hsla)> {
    [
        (n[0], "✓ ", p.green),
        (n[1], "● ", p.amber),
        (n[2], "! ", p.red),
    ]
    .into_iter()
    .map(|(n, mark, color)| {
        let color = if n > 0 { color } else { p.muted.opacity(0.6) };
        (format!("{mark}{n}").into(), color)
    })
    .collect()
}

impl PanelScene {
    /// Lays the panel out: where it is in its window now, and where each
    /// of its parts goes. Its slots are to be painted at `alpha`.
    pub(super) fn lay(
        &mut self,
        bounds: Bounds<Pixels>,
        alpha: f32,
        window: &mut Window,
        cx: &mut App,
    ) -> Laid {
        let s = window.scale_factor();
        let dev = |v: Pixels| (f32::from(v) * s).round() as i32;
        // As wide and at most as tall as the panel may be; in any other
        // window (there is none), as big as this element.
        let (room_w, room_h) =
            panel::room(cx).unwrap_or((dev(bounds.size.width), dev(bounds.size.height)));

        let counts = match self.health {
            Some((n, _)) => count_words(n, &self.p),
            _ => Vec::new(),
        };
        let health = (counts.len() == 3).then(|| {
            let w = |i: usize| width(&counts[i].0, TINY, window);
            [w(0), w(1), w(2)]
        });
        let sections = self
            .sections
            .iter()
            .map(|sec| place::SectionSpec {
                section: sec.section,
                count: sec.count,
                arrow: width(arrow(sec.open), SMALL, window),
                rows: sec.open.then(|| {
                    sec.rows
                        .iter()
                        .map(|row| place::RowSpec {
                            checkout: width(&row.checkout, TINY, window),
                            status: width(&row.status, TINY, window),
                            edge: !row.repo.is_synced() && !row.repo.remote_only,
                        })
                        .collect()
                }),
            })
            .collect();
        let mut tail = Vec::new();
        let mut tail_heights = Vec::new();
        for slot in self.tail.drain(..) {
            let (el, h) = measure(slot, room_w, s, alpha, window, cx);
            tail.push(el);
            tail_heights.push(h);
        }
        let footer = self
            .footer
            .take()
            .map(|slot| measure(slot, room_w, s, alpha, window, cx));
        let wanted = dev(-self.scroll.offset().y);
        let layout = place::layout(&place::Spec {
            scale: s,
            width: room_w,
            max_height: room_h,
            health,
            buttons: self.buttons.len(),
            sections,
            tail: tail_heights,
            footer: footer.as_ref().map(|(_, h)| *h),
            scroll: wanted,
        });
        // Scrolled past its end (rows went, a section folded): back to it.
        if layout.scroll != wanted {
            self.scroll
                .set_offset(point(px(0.0), px(-(layout.scroll as f32) / s)));
        }
        let (at, moving) = match panel::sheet(layout.size.1, window, cx) {
            Some(sheet) => (sheet.at, sheet.moving),
            None => ((dev(bounds.origin.x), dev(bounds.origin.y)), false),
        };
        // The icons its parts show, drawn by the time they are painted.
        for sec in &self.sections {
            for row in sec.rows.iter().filter(|_| sec.open) {
                crate::repo_marks::want(&row.repo, MARK, window, cx);
            }
        }
        Laid {
            layout,
            at,
            moving,
            counts,
            tail,
            footer: footer.map(|(el, _)| el),
        }
    }

    /// Everything the panel paints itself, device px from its window's
    /// corner: its sheet, and its parts in the order they are painted.
    pub(super) fn picture(&self, laid: &Laid, s: f32) -> Picture {
        let p = self.p;
        let l = &laid.layout;
        let at = laid.at;
        let sh = |r: Rect| Rect::new(r.left + at.0, r.top + at.1, r.right + at.0, r.bottom + at.1);
        let own = Key::Own;
        let mut parts = Vec::new();

        // The rim, Windows' own to the px, goes just outside the sheet.
        let sheet = Rect::new(0, 0, l.size.0, l.size.1);
        let rim_px = rim_width(s).round() as i32;
        let inner = (win::PANEL_RADIUS - rim_px as f32 / s).max(0.0);
        // The big window's title bar, of which the panel has none.
        parts.push(
            Part::fill(
                Key::Caption,
                sh(Rect::new(0, 0, l.size.0, 0)),
                title_bar(self.dark),
            )
            .radii([inner, inner, 0.0, 0.0]),
        );

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
        for (button, r) in self.buttons.iter().zip(&h.buttons) {
            let color = if button.enabled {
                p.text
            } else {
                p.text.opacity(0.4)
            };
            parts.push(
                Part::words(
                    button.key.clone(),
                    sh(centred(*r, HEADLINE, s)),
                    button.glyph.clone(),
                    Look {
                        family: button.family,
                        ..Look::new(HEADLINE, color)
                    },
                )
                .centred(),
            );
        }
        parts.push(Part::fill(Key::HeaderLine, sh(h.line), p.border));

        // The list. With no bottom bar it reaches the sheet's bottom
        // corners, rounded as they are inside the rim.
        let corners = if l.footer.is_some() { 0.0 } else { inner };
        parts
            .push(Part::fill(Key::Body, sh(l.list), p.surface).radii([0.0, 0.0, corners, corners]));
        let mut rows = l.rows.iter();
        for (sec, heading) in self.sections.iter().zip(&l.headings) {
            parts.push(
                Part::fill(
                    Key::Tint(sec.section),
                    sh(heading.rect),
                    section_tint(sec.section, &p),
                )
                .clipped(),
            );

            parts.push(
                Part::words(
                    own.clone(),
                    sh(heading.label),
                    sec.words.clone(),
                    Look::new(SMALL, sec.color),
                )
                .clipped(),
            );

            parts.push(
                Part::words(
                    own.clone(),
                    sh(heading.arrow),
                    arrow(sec.open),
                    Look::new(SMALL, p.muted),
                )
                .clipped(),
            );
            if !sec.open {
                continue;
            }
            for (part, row) in sec.rows.iter().zip(&mut rows) {
                let path = &part.repo.path;
                parts.push(
                    Part::new(
                        Key::Icon(path.clone()),
                        sh(row.icon),
                        What::Icon {
                            status: part.repo.clone(),
                            side: MARK,
                            dot: part.color,
                            ends: None,
                        },
                    )
                    .clipped(),
                );

                parts.push(
                    Part::words(
                        Key::Name(path.clone()),
                        sh(row.name),
                        part.repo.name.clone(),
                        Look::new(SMALL, p.text),
                    )
                    .clipped(),
                );
                parts.push(
                    Part::words(
                        Key::Checkout(path.clone()),
                        sh(row.checkout),
                        part.checkout.clone(),
                        Look::new(TINY, p.muted),
                    )
                    .clipped(),
                );
                parts.push(
                    Part::words(
                        Key::Status(path.clone()),
                        sh(row.status),
                        part.status.clone(),
                        Look::new(TINY, part.color),
                    )
                    .clipped(),
                );

                if let Some(edge) = row.edge {
                    parts.push(
                        Part::fill(own.clone(), sh(edge), part.color)
                            .radii([0.0, 2.0, 2.0, 0.0])
                            .over()
                            .clipped(),
                    );
                }
            }
        }
        Picture {
            sheet: Some(Sheet {
                frame: sh(sheet.outset(rim_px)),
                radius: win::PANEL_RADIUS,
                rim: rim_px,
                rim_color: rim(self.dark),
                bg: p.bg,
                shadow: 1.0,
            }),
            body: sh(l.list),
            parts,
        }
    }

    /// Where each shown repo's tree is drawn: its row's graph box, device
    /// px from the window's corner.
    pub(super) fn strips(&self, laid: &Laid) -> Vec<(PathBuf, bool, Rect)> {
        let at = laid.at;
        let mut rows = laid.layout.rows.iter();
        let mut out = Vec::new();
        for sec in self.sections.iter().filter(|sec| sec.open) {
            for (part, row) in sec.rows.iter().zip(&mut rows) {
                let r = row.strip;
                out.push((
                    part.repo.path.clone(),
                    part.repo.dirty,
                    Rect::new(r.left + at.0, r.top + at.1, r.right + at.0, r.bottom + at.1),
                ));
            }
        }
        out
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
        for (button, r) in self.buttons.iter_mut().zip(&layout.header.buttons) {
            if let Some(slot) = button.slot.take() {
                top.push(put(slot, *r, at, s, alpha, window, cx));
            }
        }

        // The list's, cut to the list: its own hitbox first, so theirs
        // lie over it.
        let list_bounds = paint::bounds(layout.list, at, s);
        let list = window.insert_hitbox(list_bounds, HitboxBehavior::Normal);
        let bleed = place::px(crate::timeline::BLEED, s) + 1;
        let in_sight =
            |r: Rect| r.bottom + bleed > layout.list.top && r.top - bleed < layout.list.bottom;
        let mut listed = Vec::new();
        let tail = std::mem::take(&mut laid.tail);
        window.with_content_mask(
            Some(ContentMask {
                bounds: list_bounds,
            }),
            |window| {
                let mut rows = layout.rows.iter();
                for (sec, heading) in self.sections.iter_mut().zip(&layout.headings) {
                    if let Some(slot) = sec.slot.take().filter(|_| in_sight(heading.rect)) {
                        listed.push(put(slot, heading.rect, at, s, alpha, window, cx));
                    }
                    if !sec.open {
                        continue;
                    }
                    for (part, row) in sec.rows.iter_mut().zip(&mut rows) {
                        if !in_sight(row.rect) {
                            continue;
                        }
                        if let Some(slot) = part.graph.take() {
                            listed.push(put(slot, row.strip, at, s, alpha, window, cx));
                        }
                        if let Some(slot) = part.label.take() {
                            listed.push(put(slot, row.label, at, s, alpha, window, cx));
                        }
                    }
                }
                for (mut el, r) in tail.into_iter().zip(&layout.tail) {
                    el.prepaint_at(paint::slot_origin(*r, at, s), window, cx);
                    listed.push(el);
                }
            },
        );
        let footer = laid.footer.take().zip(layout.footer).map(|(mut el, r)| {
            el.prepaint_at(paint::slot_origin(r, at, s), window, cx);
            el
        });
        Slots {
            top,
            listed,
            footer,
            list,
            list_bounds,
        }
    }

    /// Paints the slots; with `scroll`, the list's scroll and how far it
    /// goes, the wheel over the list scrolls it.
    pub(super) fn paint_slots(
        slots: &mut Slots,
        scroll: Option<(ScrollHandle, Pixels)>,
        window: &mut Window,
        cx: &mut App,
    ) {
        // Before the slots paint: their own wheel handlers (a strip's
        // sideways scroll) then come first.
        if let Some((scroll, max)) = scroll {
            let hitbox = slots.list.clone();
            window.on_mouse_event(move |e: &ScrollWheelEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || !hitbox.should_handle_scroll(window) {
                    return;
                }
                let d = e.delta.pixel_delta(window.line_height());
                let was = scroll.offset();
                let y = (was.y + d.y).clamp(-max, px(0.0));
                if y != was.y {
                    scroll.set_offset(point(px(0.0), y));
                    window.refresh();
                    cx.stop_propagation();
                }
            });
        }
        window.with_content_mask(
            Some(ContentMask {
                bounds: slots.list_bounds,
            }),
            |window| {
                for slot in &mut slots.listed {
                    slot.paint(window, cx);
                }
            },
        );
        for slot in &mut slots.top {
            slot.paint(window, cx);
        }
        if let Some(footer) = &mut slots.footer {
            footer.paint(window, cx);
        }
    }
}

impl IntoElement for PanelScene {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for PanelScene {
    type RequestLayoutState = ();
    type PrepaintState = Option<(Laid, Picture, Slots)>;

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
        let mut laid = self.lay(bounds, 1.0, window, cx);
        let picture = self.picture(&laid, window.scale_factor());
        let slots = self.slots(&mut laid, 1.0, window, cx);
        Some((laid, picture, slots))
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
        let Some((laid, picture, slots)) = state else {
            return;
        };
        let s = window.scale_factor();
        // The sheet with its shadow and the fills; over them the slots;
        // over those the words and pictures, and last the rim.
        picture.paint_under(window, cx);
        let max = px(laid.layout.max_scroll as f32 / s);
        Self::paint_slots(slots, Some((self.scroll.clone(), max)), window, cx);
        picture.paint_over(window, cx);
        if laid.moving {
            window.request_animation_frame();
        }
        crate::flip::painted();
        panel::drew(window, cx);
    }
}

/// A place on the top bar or in the list that takes the mouse: as big as
/// the box it is put in.
pub(super) fn hit(id: impl Into<ElementId>, live: bool, p: &Palette) -> gpui::Stateful<gpui::Div> {
    let hover = p.hover;
    div()
        .id(id)
        .size_full()
        .when(live, |d| d.cursor_pointer().hover(move |s| s.bg(hover)))
}

/// The panel's bottom bar: how it watches, what it left out, and the way
/// to its settings file.
fn footer(state: &AppState, p: &Palette) -> AnyElement {
    let config_file = state.config_file.clone();
    let text_color = p.text;
    let capped = state
        .capped
        .as_ref()
        .map(|c| format!("Repo list incomplete: {c}"));
    let skips = state.folder_skips.len();
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
                .flex()
                .flex_col()
                .min_w_0()
                .child(div().truncate().child(text(mode_text(&state.mode))))
                .when_some(capped, |d, capped| {
                    d.child(div().truncate().text_color(p.amber).child(text(capped)))
                })
                .when(skips > 0, |d| {
                    d.child(
                        div()
                            .truncate()
                            .child(text(format!("Folder icons: {skips} skipped"))),
                    )
                }),
        )
        .child(
            div()
                .id("config")
                .flex_none()
                .cursor_pointer()
                .hover(move |s| s.text_color(text_color))
                .child(text("Edit config.json"))
                .on_click(move |_, window, cx| {
                    cx.open_with_system(&config_file);
                    panel::dismiss(window, cx);
                }),
        )
        .into_any_element()
}

impl StatusView {
    /// The panel: one element that draws it, under the root that takes
    /// its keys.
    pub(super) fn render_panel(
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
        let timelines = self.timelines.clone();
        let state = self.state.clone();
        let state = state.read(cx);
        let sweeping = state.sweeping;
        let (headline, headline_color, subline) = titles(state, &p);

        // In a flip, the panel's own slots fade with it, and the flip
        // draws the trees.
        let pose = crate::flip::pose(cx);
        let staged = pose.is_some();
        let mut buttons = vec![Button {
            key: Key::Own,
            glyph: "↻".into(),
            family: None,
            enabled: !sweeping,
            slot: Some(
                hit("refresh", !sweeping, &p)
                    .rounded_md()
                    .on_click(move |_, _, cx| {
                        if !sweeping {
                            refresh(cx);
                        }
                    })
                    .into_any_element(),
            ),
        }];
        if state.config.window {
            buttons.push(Button {
                key: Key::Own,
                glyph: "⧉".into(),
                family: None,
                enabled: true,
                slot: Some(
                    hit("pop-out", true, &p)
                        .rounded_md()
                        .on_click(|_, window, cx| {
                            crate::flip::open_window(window, None, cx);
                        })
                        .into_any_element(),
                ),
            });
        }
        buttons.push(Button {
            key: Key::Settings,
            glyph: super::window_scene::GEAR.into(),
            family: Some(super::window_scene::caption_font(cx)),
            enabled: true,
            slot: Some(
                hit("settings", true, &p)
                    .rounded_md()
                    .on_click(|_, window, cx| {
                        panel::dismiss(window, cx);
                        crate::settings::show(cx);
                    })
                    .into_any_element(),
            ),
        });

        let health = (state.has_result && !state.statuses.is_empty())
            .then(|| (tone_counts(&state.statuses), sweeping));

        // The sections, each with its heading and its rows unless folded;
        // the GitHub repos with no clone come last.
        let sorted = state.sorted_for_panel();
        let (bad, good): (Vec<&RepoStatus>, Vec<&RepoStatus>) =
            sorted.into_iter().partition(|s| !s.is_synced());
        let remote = state.sorted(&state.remote_statuses);
        let no_remote = remote.is_empty();
        let mut sections = Vec::new();
        let mut index = 0;
        for (section, repos) in [
            (Section::NeedsYou, bad),
            (Section::Synced, good),
            (Section::GithubOnly, remote),
        ] {
            if repos.is_empty() {
                continue;
            }
            let n = repos.len();
            let color = section_color(section, &p);
            let open = !state.folded(section);
            let tooltip = match section {
                Section::GithubOnly => Some(vec![
                    (format!("{n} GitHub repos not cloned here"), p.text),
                    ("Their trees come from GitHub".to_string(), p.muted),
                ]),
                _ => None,
            };
            let id = match section {
                Section::NeedsYou => "needs-you",
                Section::Synced => "synced",
                _ => "remote",
            };
            let slot = hit(id, true, &p)
                .when_some(tooltip, |d, lines| d.tooltip(tip(lines, &p)))
                .on_click(fold(section))
                .into_any_element();
            let rows = if open {
                repos
                    .iter()
                    .enumerate()
                    .map(|(i, s)| {
                        let color = status_color(s, &p);
                        match &timelines {
                            Some(t) => {
                                let strip =
                                    Timelines::strip(t, index + i, s, &p, dark, !staged, false, cx);
                                RowPart {
                                    repo: Arc::new((*s).clone()),
                                    color,
                                    checkout: strip.checkout.into(),
                                    status: strip.status.into(),
                                    graph: Some(strip.graph),
                                    label: Some(strip.label),
                                }
                            }
                            // Before the app is up: the words alone.
                            None => RowPart {
                                repo: Arc::new((*s).clone()),
                                color,
                                checkout: SharedString::default(),
                                status: s.label().into(),
                                graph: None,
                                label: None,
                            },
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            };
            index += n;
            sections.push(SectionPart {
                section,
                count: n,
                open,
                color,
                words: section_text(section, n).into(),
                slot: Some(slot),
                rows,
            });
        }

        // What follows the rows: why GitHub's repos can't be read (the
        // pull requests say it when they show), the pull requests, and
        // that there is no repo here.
        let mut tail = Vec::new();
        let remote_problem = if state.config.github_prs || !state.config.github_repos {
            None
        } else {
            state.github_problem.as_deref()
        };
        if let Some(problem) = remote_problem.filter(|_| no_remote) {
            tail.push(remote_problem_row(problem, &p));
        }
        tail.extend(prs_block(state, super::Mode::Panel, &p, cx));
        let problem = state.problem();
        if state.has_result && state.statuses.is_empty() && problem.is_none() {
            let words = match state.config.depth() {
                1 => "No git repos directly inside this folder. Set \"path\" or \"depth\" in \
                      config.json."
                    .to_string(),
                depth => format!(
                    "No git repos in this folder, down to {depth} levels below it. Set \"path\" \
                     or \"depth\" in config.json."
                ),
            };
            tail.push(
                div()
                    .p_4()
                    .text_sm()
                    .text_color(p.muted)
                    .child(text(words))
                    .into_any_element(),
            );
        }

        let scene = PanelScene {
            p,
            dark,
            headline,
            headline_color,
            subline: subline.into(),
            health,
            buttons,
            sections,
            tail,
            footer: Some(footer(state, &p)),
            scroll: self.list.clone(),
        };
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
                if filter_key(&filter_state, event, super::Mode::Panel, window, cx) {
                    cx.stop_propagation();
                    return;
                }
                if event.keystroke.key == "escape" {
                    panel::dismiss(window, cx);
                }
            })
            .size_full()
            .text_color(p.text)
            .map(|root| match pose {
                // A flip: this window draws all of it.
                Some(pose) => root.child(super::stage::Stage {
                    panel: scene,
                    window: pose
                        .target
                        .map(|target| self.window_scene(Some(target), window, cx)),
                    pose,
                    p,
                    dark,
                    timelines: self.timelines.clone(),
                    laid_in: std::time::Duration::ZERO,
                }),
                None => root.child(scene).children(
                    self.timelines
                        .as_ref()
                        .and_then(|t| t.read(cx).hover_card(&p, dark, cx)),
                ),
            })
            .into_any_element()
    }
}
