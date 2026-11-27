//! The panel, drawn from the scene.
//!
//! `scene::panel` says where each part goes, in whole device px from the
//! panel's corner; this paints them there, wherever in its window the
//! panel is now (`panel::sheet`): at rest, or part way through its slide.
//! The parts that travel in the flip (the words, icons, bars and washes)
//! are painted here directly. What only the mouse or the list needs (a
//! row's tree and where its label is clicked, the buttons, the pull
//! requests, the bottom bar) are ordinary GPUI elements, the slots, laid
//! out at the device px the layout gives them and painted under the parts.

use super::{
    NEXT_LIST_SCROLL, Palette, StatusView, filter_key, fold, mode_text, prs_block, refresh,
    remote_problem_row, rim, rim_width, section_color, section_text, section_tint, status_color,
    tip, titles, tone_counts,
};
use crate::config::Section;
use crate::git::RepoStatus;
use crate::layout::Rect;
use crate::panel;
use crate::scene::paint;
use crate::scene::panel::{self as place, HEADLINE, ICON_MARK, MARK, SMALL, TINY, line_height};
use crate::scene::text::{self, Align, Look, text};
use crate::state::AppState;
use crate::timeline::Timelines;
use crate::win;
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, ContentMask, Context, DispatchPhase, Element,
    ElementId, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement,
    LayoutId, ParentElement, Pixels, ScrollHandle, ScrollWheelEvent, SharedString, Style, Styled,
    Window, WindowAppearance, div, point, prelude::*, px, relative, size,
};

/// A button on the top bar.
struct Button {
    glyph: SharedString,
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
    /// Icons only, folded: its repos, for their icons in its heading.
    marks: Vec<(RepoStatus, Hsla)>,
    rows: Vec<RowPart>,
}

/// A repo's row.
struct RowPart {
    repo: RepoStatus,
    color: Hsla,
    checkout: SharedString,
    status: SharedString,
    slot: Option<AnyElement>,
}

/// The panel as one element, as big as its window: it draws itself where
/// in that window it is to be.
pub(super) struct PanelScene {
    p: Palette,
    dark: bool,
    icons_only: bool,
    headline: SharedString,
    headline_color: Hsla,
    subline: SharedString,
    /// The health bar's counts, and whether a check runs.
    health: Option<([usize; 3], bool)>,
    /// Icons only: where the mouse reads the summary.
    summary: Option<AnyElement>,
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
    /// The slots, laid out where they go: the top bar's, the list's top
    /// to bottom, and the bottom bar.
    top: Vec<AnyElement>,
    listed: Vec<AnyElement>,
    footer: Option<AnyElement>,
    /// Which rows are in sight.
    shown: Vec<bool>,
    list: Hitbox,
}

/// `slot` in a box of its own, `r` device px from the corner at `at`,
/// laid out and ready to paint.
pub(super) fn put(
    slot: AnyElement,
    r: Rect,
    at: (i32, i32),
    s: f32,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let b = paint::bounds(r, at, s);
    let mut el = div()
        .flex()
        .flex_col()
        .w(b.size.width)
        .h(b.size.height)
        .child(slot)
        .into_any_element();
    el.prepaint_as_root(
        b.origin,
        size(
            AvailableSpace::Definite(b.size.width),
            AvailableSpace::Definite(b.size.height),
        ),
        window,
        cx,
    );
    el
}

/// `slot` laid out `width` device px wide, and how tall it comes out.
pub(super) fn measure(
    slot: AnyElement,
    width: i32,
    s: f32,
    window: &mut Window,
    cx: &mut App,
) -> (AnyElement, i32) {
    let w = px(width as f32 / s);
    let mut el = div().flex().flex_col().w(w).child(slot).into_any_element();
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

impl IntoElement for PanelScene {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for PanelScene {
    type RequestLayoutState = ();
    type PrepaintState = Option<Laid>;

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
    ) -> Option<Laid> {
        let s = window.scale_factor();
        let dev = |v: Pixels| (f32::from(v) * s).round() as i32;
        // As wide and at most as tall as the panel may be; in any other
        // window (there is none), as big as this element.
        let (room_w, room_h) =
            panel::room(cx).unwrap_or((dev(bounds.size.width), dev(bounds.size.height)));

        let counts: Vec<(SharedString, Hsla)> = match self.health {
            Some((n, _)) if !self.icons_only => {
                let p = &self.p;
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
            let (el, h) = measure(slot, room_w, s, window, cx);
            tail.push(el);
            tail_heights.push(h);
        }
        let footer = self
            .footer
            .take()
            .map(|slot| measure(slot, room_w, s, window, cx));
        let wanted = dev(-self.scroll.offset().y);
        let layout = place::layout(&place::Spec {
            scale: s,
            width: room_w,
            max_height: room_h,
            icons_only: self.icons_only,
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
        crate::morph::seen_list_scroll(layout.scroll as f32 / s, layout.max_scroll as f32 / s);

        let (at, moving) = match panel::sheet(layout.size.1, window, cx) {
            Some(sheet) => (sheet.at, sheet.moving),
            None => ((dev(bounds.origin.x), dev(bounds.origin.y)), false),
        };

        // The top bar's slots.
        let mut top = Vec::new();
        if let (Some(slot), Some(bar)) = (self.summary.take(), layout.header.bar) {
            let half = place::px(place::BUTTON, s) / 2;
            let mid = (bar.top + bar.bottom) / 2;
            let r = Rect::new(bar.left, mid - half, bar.right, mid + half);
            top.push(put(slot, r, at, s, window, cx));
        }
        for (button, r) in self.buttons.iter_mut().zip(&layout.header.buttons) {
            if let Some(slot) = button.slot.take() {
                top.push(put(slot, *r, at, s, window, cx));
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
        let mut shown = Vec::new();
        window.with_content_mask(
            Some(ContentMask {
                bounds: list_bounds,
            }),
            |window| {
                let mut rows = layout.rows.iter();
                for (sec, heading) in self.sections.iter_mut().zip(&layout.headings) {
                    if let Some(slot) = sec.slot.take().filter(|_| in_sight(heading.rect)) {
                        listed.push(put(slot, heading.rect, at, s, window, cx));
                    }
                    for ((repo, _), _) in sec.marks.iter().zip(&heading.marks) {
                        crate::repo_marks::want(repo, MARK, window, cx);
                    }
                    if !sec.open {
                        continue;
                    }
                    for (part, row) in sec.rows.iter_mut().zip(&mut rows) {
                        let sight = in_sight(row.rect);
                        shown.push(sight);
                        if !sight {
                            continue;
                        }
                        if let Some(slot) = part.slot.take() {
                            listed.push(put(slot, row.rect, at, s, window, cx));
                        }
                        let side = if self.icons_only { ICON_MARK } else { MARK };
                        crate::repo_marks::want(&part.repo, side, window, cx);
                    }
                }
                for (mut el, r) in tail.into_iter().zip(&layout.tail) {
                    el.prepaint_at(paint::bounds(*r, at, s).origin, window, cx);
                    listed.push(el);
                }
            },
        );
        let footer = footer.zip(layout.footer).map(|((mut el, _), r)| {
            el.prepaint_at(paint::bounds(r, at, s).origin, window, cx);
            el
        });
        Some(Laid {
            layout,
            at,
            moving,
            counts,
            top,
            listed,
            footer,
            shown,
            list,
        })
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        laid: &mut Option<Laid>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(laid) = laid else { return };
        let s = window.scale_factor();
        let p = self.p;
        let at = laid.at;
        let l = &laid.layout;
        let b = |r: Rect| paint::bounds(r, at, s);
        let words = |window: &mut Window, t: &SharedString, look: Look, r: Rect, align: Align| {
            text::paint(
                window,
                t,
                &look,
                (at.0 + r.left, at.1 + r.top),
                line_height(look.size),
                Some(r.width()),
                align,
            );
        };

        // The sheet with its shadow; the rim, Windows' own to the px, goes
        // on last, just outside the sheet.
        let sheet = Rect::new(0, 0, l.size.0, l.size.1);
        let rim_px = rim_width(s).round() as i32;
        let frame = b(sheet.outset(rim_px));
        let radius = px(win::PANEL_RADIUS);
        window.paint_shadows(
            frame,
            gpui::Corners::all(radius),
            &crate::morph::window_shadow(),
        );
        paint::round(window, frame, gpui::Corners::all(radius), p.bg);

        // The top bar.
        let h = &l.header;
        if !self.icons_only {
            words(
                window,
                &self.headline,
                Look::new(HEADLINE, self.headline_color).bold(),
                h.headline,
                Align::Left,
            );
            words(
                window,
                &self.subline,
                Look::new(TINY, p.muted),
                h.subline,
                Align::Left,
            );
        }
        if let (Some(bar), Some((counts, checking))) = (h.bar, self.health) {
            paint::health(
                window,
                bar,
                at,
                counts,
                [p.green, p.amber, p.red],
                p.hover,
                checking,
            );
            for ((t, color), r) in laid.counts.iter().zip(h.counts) {
                words(window, t, Look::new(TINY, *color), r, Align::Left);
            }
        }
        for slot in &mut laid.top {
            slot.paint(window, cx);
        }
        for (button, r) in self.buttons.iter().zip(&h.buttons) {
            let color = if button.enabled {
                p.text
            } else {
                p.text.opacity(0.4)
            };
            // Its glyph's line, centred on the button.
            let lh = place::px(line_height(HEADLINE), s);
            let top = r.top + (r.height() - lh) / 2;
            let line = Rect::new(r.left, top, r.right, top + lh);
            words(
                window,
                &button.glyph,
                Look::new(HEADLINE, color),
                line,
                Align::Centre,
            );
        }
        paint::fill(window, b(h.line), p.border);

        // The list. With no bottom bar it reaches the sheet's bottom
        // corners, rounded as they are inside the rim.
        let list_bounds = b(l.list);
        if l.footer.is_some() {
            paint::fill(window, list_bounds, p.surface);
        } else {
            let inner = radius - px(rim_px as f32 / s);
            let radii = gpui::Corners {
                top_left: px(0.0),
                top_right: px(0.0),
                bottom_left: inner,
                bottom_right: inner,
            };
            paint::round(window, list_bounds, radii, p.surface);
        }
        // Before the slots paint: their own wheel handlers (a strip's
        // sideways scroll) then come first.
        let (scroll, hitbox, max) = (
            self.scroll.clone(),
            laid.list.clone(),
            px(l.max_scroll as f32 / s),
        );
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
        window.with_content_mask(
            Some(ContentMask {
                bounds: list_bounds,
            }),
            |window| {
                for (sec, heading) in self.sections.iter().zip(&l.headings) {
                    paint::fill(window, b(heading.rect), section_tint(sec.section, &p));
                }
                for slot in &mut laid.listed {
                    slot.paint(window, cx);
                }
                let mut rows = l.rows.iter().zip(&laid.shown);
                for (sec, heading) in self.sections.iter().zip(&l.headings) {
                    if !self.icons_only {
                        words(
                            window,
                            &sec.words,
                            Look::new(SMALL, sec.color),
                            heading.label,
                            Align::Left,
                        );
                    }
                    for ((repo, color), r) in sec.marks.iter().zip(&heading.marks) {
                        crate::repo_marks::paint(repo, b(*r), MARK, *color, window, cx);
                    }
                    words(
                        window,
                        &arrow(sec.open).into(),
                        Look::new(SMALL, p.muted),
                        heading.arrow,
                        Align::Left,
                    );
                    if !sec.open {
                        continue;
                    }
                    for (part, (row, shown)) in sec.rows.iter().zip(&mut rows) {
                        if !shown {
                            continue;
                        }
                        let side = if self.icons_only { ICON_MARK } else { MARK };
                        crate::repo_marks::paint(
                            &part.repo,
                            b(row.icon),
                            side,
                            part.color,
                            window,
                            cx,
                        );
                        if !self.icons_only {
                            let name: SharedString = part.repo.name.clone().into();
                            words(
                                window,
                                &name,
                                Look::new(SMALL, p.text),
                                row.name,
                                Align::Left,
                            );
                            words(
                                window,
                                &part.checkout,
                                Look::new(TINY, p.muted),
                                row.checkout,
                                Align::Left,
                            );
                            words(
                                window,
                                &part.status,
                                Look::new(TINY, part.color),
                                row.status,
                                Align::Left,
                            );
                        }
                        if let Some(edge) = row.edge {
                            let r = px(2.0);
                            let radii = gpui::Corners {
                                top_left: px(0.0),
                                top_right: r,
                                bottom_right: r,
                                bottom_left: px(0.0),
                            };
                            paint::round(window, b(edge), radii, part.color);
                        }
                    }
                }
            },
        );
        if let Some(footer) = &mut laid.footer {
            footer.paint(window, cx);
        }
        window.paint_quad(gpui::quad(
            frame,
            radius,
            gpui::transparent_black(),
            px(rim_px as f32 / s),
            rim(self.dark),
            gpui::BorderStyle::Solid,
        ));
        if laid.moving {
            window.request_animation_frame();
        }
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
        // The flip lines the list up with the window's columns.
        if let Some(y) = NEXT_LIST_SCROLL.take() {
            self.list.set_offset(point(px(0.), px(-y)));
        }
        let filter_state = self.state.clone();
        let timelines = self.timelines.clone();
        let state = self.state.clone();
        let state = state.read(cx);
        let icons_only = state.config.icons_only;
        let sweeping = state.sweeping;
        let (headline, headline_color, subline) = titles(state, &p);

        let mut buttons = vec![Button {
            glyph: "↻".into(),
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
                glyph: "⧉".into(),
                enabled: true,
                slot: Some(
                    hit("pop-out", true, &p)
                        .rounded_md()
                        .on_click(|_, window, cx| {
                            crate::morph::open_window(window, None, cx);
                        })
                        .into_any_element(),
                ),
            });
        }
        buttons.push(Button {
            glyph: "⚙".into(),
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

        // Pictures only: the top bar's words move into a tooltip.
        let summary = icons_only.then(|| {
            let mut lines = vec![
                (headline.to_string(), headline_color),
                (subline.clone(), p.muted),
                (mode_text(&state.mode), p.muted),
            ];
            if let Some(c) = &state.capped {
                lines.push((format!("Repo list incomplete: {c}"), p.amber));
            }
            hit("summary", false, &p)
                .tooltip(tip(lines, &p))
                .into_any_element()
        });
        let health = (icons_only || (state.has_result && !state.statuses.is_empty()))
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
                Section::NeedsYou if icons_only => Some(vec![(format!("{n} need you"), p.red)]),
                Section::Synced if icons_only => Some(vec![(format!("{n} synced"), p.green)]),
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
            let marks = if icons_only && !open {
                repos
                    .iter()
                    .map(|s| {
                        let mark = match section {
                            Section::NeedsYou => status_color(s, &p),
                            _ => color,
                        };
                        ((*s).clone(), mark)
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let rows = if open {
                repos
                    .iter()
                    .enumerate()
                    .map(|(i, s)| {
                        let color = status_color(s, &p);
                        match &timelines {
                            Some(t) => {
                                let strip =
                                    Timelines::strip(t, index + i, s, &p, dark, icons_only, cx);
                                RowPart {
                                    repo: (*s).clone(),
                                    color,
                                    checkout: strip.checkout.into(),
                                    status: strip.status.into(),
                                    slot: Some(strip.slot),
                                }
                            }
                            // Before the app is up: the words alone.
                            None => RowPart {
                                repo: (*s).clone(),
                                color,
                                checkout: SharedString::default(),
                                status: s.label().into(),
                                slot: None,
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
                marks,
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
        tail.extend(prs_block(state, super::Mode::Panel, icons_only, &p, cx));
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
            icons_only,
            headline,
            headline_color,
            subline: subline.into(),
            health,
            summary,
            buttons,
            sections,
            tail,
            footer: (!icons_only).then(|| footer(state, &p)),
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
            .child(scene)
            .children(
                self.timelines
                    .as_ref()
                    .and_then(|t| t.read(cx).hover_card(&p, dark, cx)),
            )
            .into_any_element()
    }
}
