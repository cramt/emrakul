//! The pointer's cursor: the client's own cursor surface when it sets one,
//! an arrow drawn here when it asks for a named cursor, and nothing when it
//! hides it (Chromium does, over a playing video).

use smithay::{
    backend::renderer::{
        element::{
            Kind,
            memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
            surface::render_elements_from_surface_tree,
        },
        gles::GlesRenderer,
    },
    input::pointer::{CursorImageStatus, CursorImageSurfaceData},
    utils::{Logical, Physical, Point, Scale},
    wayland::compositor::with_states,
};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

use crate::{
    drm::{self, Element},
    home,
};

/// The arrow's outline, in its own pixels, tip at the origin.
const ARROW: [(f32, f32); 7] = [
    (0.0, 0.0),
    (0.0, 60.0),
    (15.0, 46.0),
    (25.0, 69.0),
    (35.0, 64.0),
    (24.0, 43.0),
    (44.0, 43.0),
];
/// Room around the arrow for its outline, which puts the tip here.
const MARGIN: f32 = 4.0;

pub struct Cursor {
    pub status: CursorImageStatus,
    arrow: MemoryRenderBuffer,
}

impl Cursor {
    pub fn new() -> Self {
        Self {
            status: CursorImageStatus::default_named(),
            arrow: home::buffer(&arrow(), false),
        }
    }

    /// The cursor's elements with its hotspot at `at`, front to back. A
    /// client's own cursor surface is drawn at the output's `scale`, like
    /// the rest of that client; the arrow is the same size at any scale.
    pub fn elements(
        &self,
        renderer: &mut GlesRenderer,
        at: Point<f64, Logical>,
        scale: i32,
    ) -> Vec<Element> {
        match &self.status {
            CursorImageStatus::Hidden => Vec::new(),
            CursorImageStatus::Surface(surface) => {
                let hotspot = with_states(surface, |states| {
                    states
                        .data_map
                        .get::<CursorImageSurfaceData>()
                        .map(|data| data.lock().unwrap().hotspot)
                })
                .unwrap_or_default();
                let scale = Scale::from(f64::from(scale));
                render_elements_from_surface_tree(
                    renderer,
                    surface,
                    (at - hotspot.to_f64()).to_physical_precise_round(scale),
                    scale,
                    1.0,
                    Kind::Cursor,
                )
            }
            CursorImageStatus::Named(_) => MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                arrow_location(at, scale),
                &self.arrow,
                None,
                None,
                None,
                Kind::Cursor,
            )
            .inspect_err(|err| tracing::warn!(?err, "uploading the cursor"))
            .map(|arrow| drm::unscaled(arrow, scale))
            .into_iter()
            .collect(),
        }
    }
}

/// Where the arrow's buffer goes for the pointer at `at`: its tip on the
/// screen pixel a client at `scale` draws its `at` on.
fn arrow_location(at: Point<f64, Logical>, scale: i32) -> Point<f64, Physical> {
    at.to_physical(f64::from(scale)) - Point::from((f64::from(MARGIN), f64::from(MARGIN)))
}

/// A white arrow with a dark outline, big enough to find on a TV across
/// the room.
fn arrow() -> Pixmap {
    let (w, h) = ARROW
        .iter()
        .fold((0.0f32, 0.0f32), |(w, h), (x, y)| (w.max(*x), h.max(*y)));
    let mut pixmap = Pixmap::new(
        (w + 2.0 * MARGIN).ceil() as u32,
        (h + 2.0 * MARGIN).ceil() as u32,
    )
    .expect("a non-empty size");
    let mut path = PathBuilder::new();
    let [(x, y), rest @ ..] = ARROW;
    path.move_to(x, y);
    for (x, y) in rest {
        path.line_to(x, y);
    }
    path.close();
    let path = path.finish().expect("a closed polygon");
    let at = Transform::from_translate(MARGIN, MARGIN);
    let mut paint = Paint::default();
    paint.set_color_rgba8(255, 255, 255, 255);
    pixmap.fill_path(&path, &paint, FillRule::Winding, at, None);
    paint.set_color_rgba8(20, 20, 20, 255);
    let stroke = Stroke {
        width: 4.0,
        ..Stroke::default()
    };
    pixmap.stroke_path(&path, &paint, &stroke, at, None);
    pixmap
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_arrow_tip_is_drawn_where_the_client_sees_the_pointer() {
        // A client at scale 2 draws its logical (1440, 540) at the TV's
        // (2880, 1080). The arrow's buffer starts MARGIN up and left of
        // its tip, and stays its own size in screen pixels.
        assert_eq!(
            arrow_location((1440.0, 540.0).into(), 2),
            Point::from((2876.0, 1076.0))
        );
    }
}
