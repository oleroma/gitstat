//! The flip, drawn in the panel's window.
//!
//! The panel and the big window each lay themselves out and list what
//! they paint (`scene::parts`), the big window where its own window is on
//! the screen. This paints the list part way between the two, the trees
//! part way through their turn (`timeline::paint_turning`), and each
//! view's slots while that view's own parts show. At either end the list
//! is that view's, its slots are whole and its trees its own: the frame
//! is the view's own frame.

use super::Palette;
use super::panel_scene::{self, PanelScene};
use super::window_scene::{self, WindowScene};
use crate::flip::Pose;
use crate::layout::Rect;
use crate::scene::paint;
use crate::scene::parts::Picture;
use crate::timeline::{Timelines, Turning, paint_turning};
use gpui::{
    App, Bounds, ContentMask, Element, ElementId, Entity, GlobalElementId, HitboxBehavior,
    InspectorElementId, IntoElement, LayoutId, Pixels, Style, Window, relative,
};

pub(super) struct Stage {
    pub panel: PanelScene,
    /// The big window, once it is known where it is.
    pub window: Option<WindowScene>,
    pub pose: Pose,
    pub p: Palette,
    pub dark: bool,
    pub timelines: Option<Entity<Timelines>>,
    /// How long this frame's layout took, for the flip's log.
    pub laid_in: std::time::Duration,
}

/// The flip laid out for this frame.
pub(super) struct Laid {
    picture: Picture,
    trees: Vec<Turning>,
    panel: Option<panel_scene::Slots>,
    window: Option<window_scene::Slots>,
}

impl IntoElement for Stage {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Stage {
    type RequestLayoutState = ();
    type PrepaintState = Laid;

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
    ) -> Laid {
        let s = window.scale_factor();
        let pose = self.pose;
        let began = std::time::Instant::now();
        let mut panel_laid = self.panel.lay(bounds, pose.panel, window, cx);
        let panel_picture = self.panel.picture(&panel_laid, s);
        let mut window_laid = self
            .window
            .as_mut()
            .map(|scene| scene.lay(bounds, pose.window, window, cx));
        let window_picture = self
            .window
            .as_ref()
            .zip(window_laid.as_ref())
            .map(|(scene, laid)| scene.picture(laid, s, None));
        let picture = match &window_picture {
            Some(to) => Picture::between(&panel_picture, to, pose.turn, pose.panel, pose.window),
            // Not yet known where the window is: the panel, what only it
            // shows fading.
            None => Picture::between(&panel_picture, &panel_picture, 0.0, pose.panel, 0.0),
        };
        picture.want(window, cx);
        if let (n @ 1.., Some((a, b))) = picture.clashes() {
            crate::flip::note(format!("{n} words over one another, as {a:?} and {b:?}"));
        }

        // The trees: each repo's that either view shows, from its strip
        // to its column.
        let b = |r: Rect| paint::bounds(r, (0, 0), s);
        let meets = |a: Rect, b: Rect| {
            a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
        };
        let bodies = self
            .window
            .as_ref()
            .zip(window_laid.as_ref())
            .map(|(scene, laid)| scene.bodies(laid))
            .unwrap_or_default();
        let (list, table) = (panel_picture.body, window_picture.as_ref().map(|p| p.body));
        let mut trees = Vec::new();
        if let Some(timelines) = &self.timelines {
            let timelines = timelines.read(cx);
            for (path, dirty, strip) in self.panel.strips(&panel_laid) {
                let column = bodies.iter().find(|(p, _)| *p == path).map(|(_, r)| *r);
                let shown = meets(strip, list)
                    || column
                        .zip(table)
                        .is_some_and(|(column, table)| meets(column, table));
                // A tree with no column to turn to (the window isn't
                // known yet) stays its strip's.
                if !shown || (column.is_none() && pose.turn > 0.0) {
                    continue;
                }
                let Some(graph) = timelines.graph_of(&path) else {
                    continue;
                };
                trees.push(Turning {
                    graph,
                    dirty,
                    strip: b(strip),
                    column: b(column.unwrap_or(strip)),
                });
            }
        }

        // Each view's slots, while its own parts show.
        let panel =
            (pose.panel > 0.0).then(|| self.panel.slots(&mut panel_laid, pose.panel, window, cx));
        let slots = match (&mut self.window, &mut window_laid) {
            (Some(scene), Some(laid)) if pose.window > 0.0 => {
                Some(scene.slots(laid, pose.window, window, cx))
            }
            _ => None,
        };
        // Nothing in a flip takes the mouse.
        window.insert_hitbox(bounds, HitboxBehavior::BlockMouse);
        self.laid_in = began.elapsed();
        Laid {
            picture,
            trees,
            panel,
            window: slots,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        laid: &mut Laid,
        window: &mut Window,
        cx: &mut App,
    ) {
        let s = window.scale_factor();
        let began = std::time::Instant::now();
        // As each view paints itself: the sheet and the fills; the trees
        // and the slots; the words and pictures, and last the rim.
        laid.picture.paint_under(window, cx);
        let body = paint::bounds(laid.picture.body, (0, 0), s);
        window.with_content_mask(Some(ContentMask { bounds: body }), |window| {
            paint_turning(&laid.trees, self.pose.turn, self.dark, &self.p, window);
        });
        if let Some(slots) = &mut laid.panel {
            PanelScene::paint_slots(slots, None, window, cx);
        }
        if let Some(slots) = &mut laid.window {
            WindowScene::paint_slots(slots, window, cx);
        }
        laid.picture.paint_over(window, cx);
        crate::flip::took(self.laid_in, began.elapsed());
        crate::flip::painted();
        crate::panel::drew(window, cx);
        crate::flip::drew(&self.pose, window, cx);
    }
}
