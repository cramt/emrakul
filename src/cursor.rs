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
    utils::{Logical, Point, Scale},
    wayland::compositor::with_states,
};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

use crate::{drm::Element, home};

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

    /// The cursor's elements with its hotspot at `at`, front to back.
    pub fn elements(&self, renderer: &mut GlesRenderer, at: Point<f64, Logical>) -> Vec<Element> {
        let scale = Scale::from(1.0);
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
                render_elements_from_surface_tree(
                    renderer,
                    surface,
                    (at - hotspot.to_f64()).to_physical_precise_round(scale),
                    scale,
                    1.0,
                    Kind::Cursor,
                )
            }
            CursorImageStatus::Named(_) => {
                let at = at - Point::from((f64::from(MARGIN), f64::from(MARGIN)));
                MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    at.to_physical(scale),
                    &self.arrow,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                )
                .inspect_err(|err| tracing::warn!(?err, "uploading the cursor"))
                .map(Element::from)
                .into_iter()
                .collect()
            }
        }
    }
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
