//! The big window, drawn from the scene.
//!
//! `scene::window` says where each part goes, in whole device px from the
//! window's corner; this paints them there. gitstat draws the title bar
//! itself: its colour, icon, name and three buttons, which Windows still
//! works (dragging, snapping, minimising, closing) through the places
//! this marks for it. The top bar's words, bar and buttons are painted
//! here directly. The table's cells (a section's spine, a repo's column),
//! the pull requests and the bottom bar are ordinary GPUI elements, the
//! slots, laid out at the device px the layout gives them.

use super::panel_scene::{hit, measure, put, width};
use super::{
    Cell, Mode, NEXT_TABLE_SCROLL, Palette, StatusView, cell_of, filter_key, prs_block, refresh,
    repo_row, spine, table_cells, title_bar, titles, tone_counts,
};
use crate::layout::Rect;
use crate::scene::paint;
use crate::scene::panel::{HEADLINE as GLYPH, SMALL, TINY, line_height};
use crate::scene::text::{self, Align, Look, text};
use crate::scene::window::{self as place, CellSpec, HEADLINE};
use crate::state::AppState;
use crate::timeline::Timelines;
use gpui::{
    AnyElement, App, Bounds, ContentMask, Context, DispatchPhase, Element, ElementId,
    GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId,
    MouseExitEvent, MouseMoveEvent, ParentElement, Pixels, ScrollHandle, ScrollWheelEvent,
    SharedString, Style, Styled, Window, WindowAppearance, WindowControlArea, div, point,
    prelude::*, px, relative,
};

/// The title bar's glyphs' size, logical px, as Windows 11 draws them.
const CAPTION_GLYPH: f32 = 10.0;
/// The top of the title bar left to Windows, logical px: where the mouse
/// sizes the window by its top edge.
const TOP_GRIP: f32 = 4.0;
/// The close button under the mouse.
const CLOSE_HOVER: u32 = 0xc42b1c;

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

/// The big window as one element, as big as its window.
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
    /// The table's cells, each with its slot when it is in sight.
    cells: Vec<(CellSpec, Option<AnyElement>)>,
    /// What shows in the table's place when there is no table.
    body: Option<AnyElement>,
    prs: Option<AnyElement>,
    footer: Option<AnyElement>,
    scroll: ScrollHandle,
}

/// The window laid out for this frame.
pub(super) struct Laid {
    layout: place::Layout,
    counts: Vec<(SharedString, Hsla)>,
    top: Vec<AnyElement>,
    cells: Vec<AnyElement>,
    prs: Option<AnyElement>,
    footer: Option<AnyElement>,
    /// The whole window, for the wheel, and the title bar's buttons.
    whole: Hitbox,
    drag: Hitbox,
    buttons: [Hitbox; 3],
}

impl IntoElement for WindowScene {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for WindowScene {
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
        let at = (0, 0);
        let (w, h) = (dev(bounds.size.width), dev(bounds.size.height));

        let counts: Vec<(SharedString, Hsla)> = match self.health {
            Some((n, _)) => {
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
            None => Vec::new(),
        };
        let health = (counts.len() == 3).then(|| {
            let cw = |i: usize| width(&counts[i].0, TINY, window);
            [cw(0), cw(1), cw(2)]
        });
        let prs = self.prs.take().map(|slot| measure(slot, w, s, window, cx));
        let footer = self
            .footer
            .take()
            .map(|slot| measure(slot, w, s, window, cx));
        let wanted = dev(-self.scroll.offset().x);
        let layout = place::layout(&place::Spec {
            scale: s,
            size: (w, h),
            health,
            refresh: width(&self.refresh_words, SMALL, window),
            cells: self.cells.iter().map(|(kind, _)| *kind).collect(),
            scroll: wanted,
            prs: prs.as_ref().map(|(_, tall)| *tall),
            footer: footer.as_ref().map_or(0, |(_, tall)| *tall),
        });
        // Scrolled past its end (the window grew, a section folded): back
        // to it.
        if layout.scroll != wanted {
            self.scroll
                .set_offset(point(px(-(layout.scroll as f32) / s), px(0.0)));
        }

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
        let cap = &layout.caption;
        let drag = window.insert_hitbox(
            paint::bounds(under_grip(cap.rect), at, s),
            HitboxBehavior::Normal,
        );
        let buttons = cap.buttons.map(|r| {
            window.insert_hitbox(paint::bounds(under_grip(r), at, s), HitboxBehavior::Normal)
        });
        // The top bar's slots.
        let mut top = Vec::new();
        if let Some(slot) = self.settings.take() {
            top.push(put(slot, layout.header.settings, at, s, window, cx));
        }
        if let Some(slot) = self.refresh.take() {
            top.push(put(slot, layout.header.refresh, at, s, window, cx));
        }

        // The table's, cut to the table; or what shows in its place.
        let table_bounds = paint::bounds(layout.table, at, s);
        let mut cells = Vec::new();
        window.with_content_mask(
            Some(ContentMask {
                bounds: table_bounds,
            }),
            |window| {
                if let Some(slot) = self.body.take() {
                    cells.push(put(slot, layout.table, at, s, window, cx));
                    return;
                }
                for ((_, slot), r) in self.cells.iter_mut().zip(&layout.cells) {
                    if let Some(slot) = slot.take()
                        && r.right > 0
                        && r.left < w
                    {
                        cells.push(put(slot, *r, at, s, window, cx));
                    }
                }
            },
        );
        let prs = prs.zip(layout.prs).map(|((mut el, _), r)| {
            el.prepaint_at(paint::bounds(r, at, s).origin, window, cx);
            el
        });
        let footer = footer.map(|(mut el, _)| {
            el.prepaint_at(paint::bounds(layout.footer, at, s).origin, window, cx);
            el
        });
        Some(Laid {
            layout,
            counts,
            top,
            cells,
            prs,
            footer,
            whole,
            drag,
            buttons,
        })
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        laid: &mut Option<Laid>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(laid) = laid else { return };
        let s = window.scale_factor();
        let p = self.p;
        let at = (0, 0);
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
        // A glyph's line, centred on its button.
        let centred = |r: Rect, size: f32| {
            let lh = place::px(line_height(size), s);
            let top = r.top + (r.height() - lh) / 2;
            Rect::new(r.left, top, r.right, top + lh)
        };
        paint::fill(window, bounds, p.bg);

        // The title bar. Windows asks which of its parts the mouse is on:
        // the three buttons first, then the bar that drags the window.
        for (area, hitbox) in [
            WindowControlArea::Min,
            WindowControlArea::Max,
            WindowControlArea::Close,
        ]
        .into_iter()
        .zip(&laid.buttons)
        {
            window.insert_window_control_hitbox(area, hitbox.clone());
        }
        window.insert_window_control_hitbox(WindowControlArea::Drag, laid.drag.clone());
        let cap = &l.caption;
        paint::fill(window, b(cap.rect), title_bar(self.dark));
        paint::app_icon(window, cap.icon, at);
        words(
            window,
            &"gitstat".into(),
            Look::new(TINY, p.text),
            cap.title,
            Align::Left,
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
        for (i, ((glyph, r), hitbox)) in glyphs
            .into_iter()
            .zip(cap.buttons)
            .zip(&laid.buttons)
            .enumerate()
        {
            let hovered = hitbox.is_hovered(window);
            let mut color = p.text;
            if hovered {
                let wash: Hsla = match (i, self.dark) {
                    (2, _) => {
                        color = gpui::white();
                        gpui::rgb(CLOSE_HOVER).into()
                    }
                    (_, true) => gpui::white().opacity(0.06),
                    (_, false) => gpui::black().opacity(0.04),
                };
                paint::fill(window, b(r), wash);
            }
            words(
                window,
                &glyph.into(),
                Look::new(CAPTION_GLYPH, color).family(self.font),
                centred(r, CAPTION_GLYPH),
                Align::Centre,
            );
            // Drawn again when the mouse comes onto it or leaves it.
            let over = hitbox.clone();
            window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, _| {
                if phase == DispatchPhase::Capture && over.is_hovered(window) != hovered {
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

        // The top bar.
        let h = &l.header;
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
        let dim = |c: Hsla| if self.checking { c.opacity(0.6) } else { c };
        paint::round(
            window,
            b(h.refresh),
            gpui::Corners::all(px(6.0)),
            dim(p.button),
        );
        for slot in &mut laid.top {
            slot.paint(window, cx);
        }
        words(
            window,
            &"⚙".into(),
            Look::new(GLYPH, p.text),
            centred(h.settings, GLYPH),
            Align::Centre,
        );
        words(
            window,
            &self.refresh_words,
            Look::new(SMALL, dim(p.button_text)),
            h.refresh_words,
            Align::Left,
        );
        paint::fill(window, b(h.line), p.border);

        // The table. The wheel anywhere scrolls it sideways, unless what
        // is under the mouse takes it first (a column's commits, the pull
        // requests): registered before the slots paint, it comes last.
        let table_bounds = b(l.table);
        paint::fill(window, table_bounds, p.surface);
        let (scroll, whole, max) = (
            self.scroll.clone(),
            laid.whole.clone(),
            px(l.max_scroll as f32 / s),
        );
        let prs_bounds = l.prs.map(b);
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
        window.with_content_mask(
            Some(ContentMask {
                bounds: table_bounds,
            }),
            |window| {
                for slot in &mut laid.cells {
                    slot.paint(window, cx);
                }
            },
        );
        if let Some(prs) = &mut laid.prs {
            prs.paint(window, cx);
        }
        if let Some(footer) = &mut laid.footer {
            footer.paint(window, cx);
        }
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
        let s = window.scale_factor();
        let filter_state = self.state.clone();
        let timelines = self.timelines.clone();
        // A strip's label asked for its repo's column.
        let focus = self
            .timelines
            .as_ref()
            .and_then(|t| t.update(cx, |t, _| t.take_focus()));
        // Set by the flip, which has brought any asked-for column into
        // sight already.
        let table_to = NEXT_TABLE_SCROLL.take();
        let state = self.state.clone();
        let state = state.read(cx);
        let sweeping = state.sweeping;
        let (headline, headline_color, subline) = titles(state, &p);
        let health = (state.has_result && !state.statuses.is_empty())
            .then(|| (tone_counts(&state.statuses), sweeping));

        let settings = hit("settings", true, &p)
            .rounded_md()
            .on_click(|_, _, cx| crate::settings::show(cx))
            .into_any_element();
        let lighter = gpui::white().opacity(0.1);
        let refresh_slot = div()
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
            .into_any_element();

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
                        Cell::Repo(_) => CellSpec::Column,
                    })
                    .collect();
                let edges = place::cell_edges(&kinds, s);
                let view = (f32::from(window.viewport_size().width) * s).round() as i32;
                let max = (edges.last().copied().unwrap_or(0) - view).max(0);
                let mut scroll = match table_to {
                    Some(x) => (x * s).round() as i32,
                    None => (-f32::from(self.table.offset().x) * s).round() as i32,
                };
                // The asked-for column, wholly in sight.
                if let Some(i) = focus
                    .as_deref()
                    .and_then(|f| cell_of(&all, f))
                    .filter(|_| table_to.is_none())
                {
                    scroll = scroll.max(edges[i + 1] - view).min(edges[i]);
                }
                let scroll = scroll.clamp(0, max);
                self.table
                    .set_offset(point(px(-(scroll as f32) / s), px(0.0)));
                for (i, (cell, kind)) in all.iter().zip(kinds).enumerate() {
                    let seen = edges[i + 1] > scroll && edges[i] < scroll + view;
                    let slot = seen.then(|| match *cell {
                        Cell::Spine(section, n) => spine(section, n, state.folded(section), &p),
                        Cell::Repo(repo) => Timelines::column(t, i, repo, &p, dark, window, cx),
                    });
                    cells.push((kind, slot));
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

        let scene = WindowScene {
            p,
            dark,
            maximized: window.is_maximized(),
            font: caption_font(cx),
            headline,
            headline_color,
            subline: subline.into(),
            health,
            settings: Some(settings),
            refresh: Some(refresh_slot),
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
            scroll: self.table.clone(),
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
                if filter_key(&filter_state, event, Mode::Window, window, cx) {
                    cx.stop_propagation();
                    return;
                }
                if event.keystroke.key == "escape" && !crate::morph::flip_back(window, cx) {
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
