//! Home drawn as the webOS ribbon: the focused app's name large over a
//! backdrop, and one row of tiles along the bottom.
//!
//! Laid out in the TV's physical pixels at 3840x2160, scale 1, sizes taken
//! from the prototype (docs/prototypes/home, layout A) at twice its CSS px.
//!
//! Everything is rasterised on the CPU once into its own
//! `MemoryRenderBuffer` (see docs/research on the research/compositor-ui-drawing
//! branch): a tile per app in each of its two looks, the focused app's
//! title, and a backdrop. Moving focus swaps two tiles and the title, so the
//! damage tracker repaints just those, and an untouched Home costs nothing.

use std::{collections::HashMap, path::PathBuf};

use anyhow::Context;
use cosmic_text::{
    Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Weight, Wrap, fontdb,
};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
            },
            gles::GlesRenderer,
        },
    },
    utils::{Rectangle, Transform},
};
use tiny_skia::{
    FillRule, FilterQuality, Paint, Path, PathBuilder, Pixmap, PixmapPaint, PremultipliedColorU8,
    Shader, Stroke,
};

use crate::{
    apps::{self, App, AppId, Rgb},
    icons::{self, IconFile},
    lifecycle::Home,
};

/// Picked in flake.nix and baked in as a store path, so the binary carries
/// its font and Home looks the same whatever fonts the machine has.
const FONT: &str = env!("EMRAKUL_FONT");

const SCREEN_W: i32 = 3840;
const SCREEN_H: i32 = 2160;
const ROW_LEFT: i32 = 192;
const ROW_BOTTOM: i32 = 1920;
const TILE: i32 = 440;
const FOCUSED_TILE: i32 = 560;
const GAP: i32 = 56;
/// How many tiles stay left of the focus before the row scrolls, so the
/// next few apps always peek in from the right.
const LEFT_OF_FOCUS: usize = 4;
const RADIUS: f32 = 36.0;
/// The focus ring: this wide, this far outside the focused tile.
const RING_WIDTH: i32 = 12;
const RING_GAP: i32 = 12;
const RING_OUT: i32 = RING_WIDTH + RING_GAP;
/// The icon's share of its tile's side.
const ICON_SHARE: f32 = 0.56;
const TITLE_TOP: i32 = 640;
const TITLE_SIZE: f32 = 192.0;
const LETTER_SIZE: f32 = 128.0;
/// The backdrop is a soft gradient, so it is drawn this many times smaller
/// and stretched: an eighth of the upload, and no visible difference.
const BACKDROP_SIZE: (i32, i32) = (SCREEN_W / 8, SCREEN_H / 8);

const BACKGROUND: Rgb = Rgb([0x0d, 0x0f, 0x14]);
const GLOW: Rgb = Rgb([0x2a, 0x35, 0x50]);
const TILE_COLOUR: Rgb = Rgb([0x1c, 0x20, 0x29]);
const LETTER_COLOUR: Rgb = Rgb([0x8a, 0x93, 0xa8]);
const TITLE_COLOUR: Rgb = Rgb([0xe8, 0xea, 0xf0]);
const RING_COLOUR: Rgb = Rgb([0xff, 0xff, 0xff]);

/// One tile on screen: which app, and its left edge. Tiles sit on a common
/// bottom line, so the focused one grows upwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    pub index: usize,
    pub x: i32,
    pub focused: bool,
}

/// The tiles that land on screen, left to right.
pub fn ribbon(count: usize, focus: usize) -> impl Iterator<Item = Placed> {
    let first = focus.saturating_sub(LEFT_OF_FOCUS);
    (first..count)
        .scan(ROW_LEFT, move |x, index| {
            let focused = index == focus;
            let placed = Placed {
                index,
                x: *x,
                focused,
            };
            *x += if focused { FOCUSED_TILE } else { TILE } + GAP;
            Some(placed)
        })
        .take_while(|placed| placed.x < SCREEN_W)
}

type Element = MemoryRenderBufferRenderElement<GlesRenderer>;

pub struct View {
    fonts: FontSystem,
    swash: SwashCache,
    data_dirs: Vec<PathBuf>,
    backdrop: MemoryRenderBuffer,
    /// Rasterised the first time each app's tile comes on screen.
    tiles: HashMap<(AppId, bool), MemoryRenderBuffer>,
    /// Only the focused app's: titles are big, and re-rasterising one on a
    /// focus move takes a few milliseconds.
    title: Option<(AppId, MemoryRenderBuffer)>,
}

impl View {
    pub fn new() -> anyhow::Result<Self> {
        let mut db = fontdb::Database::new();
        db.load_font_file(FONT)
            .with_context(|| format!("loading {FONT}"))?;
        Ok(Self {
            fonts: FontSystem::new_with_locale_and_db("en-US".into(), db),
            swash: SwashCache::new(),
            data_dirs: apps::data_dirs(),
            backdrop: buffer(&backdrop(), true),
            tiles: HashMap::new(),
            title: None,
        })
    }

    /// Drops every app's tiles and title, for when the app list is read
    /// again and an icon or brand may have changed.
    pub fn forget_apps(&mut self) {
        self.tiles.clear();
        self.title = None;
    }

    /// Home's elements, front to back.
    pub fn elements(&mut self, renderer: &mut GlesRenderer, home: &Home) -> Vec<Element> {
        let mut elements = Vec::new();
        let mut push =
            |buffer: &MemoryRenderBuffer, x: i32, y: i32, stretch: Option<(i32, i32)>| {
                // A stretched buffer needs its own size as the source, or
                // Smithay crops it to the target size instead of scaling it.
                let src =
                    stretch.map(|(w, h)| Rectangle::from_size((f64::from(w), f64::from(h)).into()));
                match Element::from_buffer(
                    renderer,
                    (f64::from(x), f64::from(y)),
                    buffer,
                    None,
                    src,
                    stretch.map(|_| (SCREEN_W, SCREEN_H).into()),
                    Kind::Unspecified,
                ) {
                    Ok(element) => elements.push(element),
                    Err(err) => tracing::warn!(?err, "uploading part of Home"),
                }
            };

        if let Some(app) = home.apps.get(home.focus) {
            if self.title.as_ref().is_none_or(|(id, _)| *id != app.id) {
                let title = text(
                    &mut self.fonts,
                    &mut self.swash,
                    &app.name,
                    Family::Name("Inter Display"),
                    TITLE_SIZE,
                    TITLE_COLOUR,
                    SCREEN_W - 2 * ROW_LEFT,
                );
                self.title = Some((app.id.clone(), buffer(&title, false)));
            }
            if let Some((_, title)) = &self.title {
                push(title, ROW_LEFT, TITLE_TOP, None);
            }
        }

        for placed in ribbon(home.apps.len(), home.focus) {
            let app = &home.apps[placed.index];
            let key = (app.id.clone(), placed.focused);
            if !self.tiles.contains_key(&key) {
                let pixmap = tile(
                    &mut self.fonts,
                    &mut self.swash,
                    &self.data_dirs,
                    app,
                    placed.focused,
                );
                self.tiles.insert(key.clone(), buffer(&pixmap, false));
            }
            let (x, y) = if placed.focused {
                (placed.x - RING_OUT, ROW_BOTTOM - FOCUSED_TILE - RING_OUT)
            } else {
                (placed.x, ROW_BOTTOM - TILE)
            };
            push(&self.tiles[&key], x, y, None);
        }

        push(&self.backdrop, 0, 0, Some(BACKDROP_SIZE));
        elements
    }
}

fn buffer(pixmap: &Pixmap, opaque: bool) -> MemoryRenderBuffer {
    let size = (pixmap.width() as i32, pixmap.height() as i32);
    // tiny-skia's premultiplied RGBA bytes are exactly ABGR8888 in DRM's
    // little-endian naming.
    MemoryRenderBuffer::from_slice(
        pixmap.data(),
        Fourcc::Abgr8888,
        size,
        1,
        Transform::Normal,
        opaque.then(|| vec![Rectangle::from_size(size.into())]),
    )
}

/// The prototype's backdrop: a glow up and to the left, fading to the
/// background, which the row of tiles sits on.
fn backdrop() -> Pixmap {
    let (w, h) = BACKDROP_SIZE;
    let mut pixmap = Pixmap::new(w as u32, h as u32).expect("a non-empty size");
    let (w, h) = (w as f32, h as f32);
    let centre = tiny_skia::Point::from_xy(w * 0.3, h * 0.2);
    let glow = tiny_skia::RadialGradient::new(
        centre,
        0.0,
        centre,
        w * 0.7,
        vec![
            tiny_skia::GradientStop::new(0.0, colour(GLOW)),
            tiny_skia::GradientStop::new(1.0, colour(BACKGROUND)),
        ],
        tiny_skia::SpreadMode::Pad,
        tiny_skia::Transform::identity(),
    );
    match glow {
        Some(shader) => {
            let paint = Paint {
                shader,
                ..Paint::default()
            };
            pixmap.fill_rect(
                tiny_skia::Rect::from_xywh(0.0, 0.0, w, h).expect("a non-empty size"),
                &paint,
                tiny_skia::Transform::identity(),
                None,
            );
        }
        None => pixmap.fill(colour(BACKGROUND)),
    }
    pixmap
}

/// An app's tile: its icon centred on its brand colour, with the focus ring
/// around it when focused. A focused tile's pixmap includes the ring, so it
/// is `RING_OUT` larger on every side than the tile itself.
fn tile(
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    data_dirs: &[PathBuf],
    app: &App,
    focused: bool,
) -> Pixmap {
    let (side, inset) = if focused {
        (FOCUSED_TILE + 2 * RING_OUT, RING_OUT)
    } else {
        (TILE, 0)
    };
    let mut pixmap = Pixmap::new(side as u32, side as u32).expect("a non-empty size");
    let size = side - 2 * inset;
    pixmap.fill_path(
        &rounded_rect(inset as f32, inset as f32, size as f32, RADIUS),
        &solid(app.brand.unwrap_or(TILE_COLOUR)),
        FillRule::Winding,
        tiny_skia::Transform::identity(),
        None,
    );
    if focused {
        let half = RING_WIDTH as f32 / 2.0;
        pixmap.stroke_path(
            &rounded_rect(
                half,
                half,
                side as f32 - 2.0 * half,
                RADIUS + RING_OUT as f32 - half,
            ),
            &solid(RING_COLOUR),
            &Stroke {
                width: RING_WIDTH as f32,
                ..Stroke::default()
            },
            tiny_skia::Transform::identity(),
            None,
        );
    }

    let box_side = size as f32 * ICON_SHARE;
    let icon = app
        .icon
        .as_deref()
        .and_then(|name| icons::find(name, data_dirs))
        .and_then(|file| match icon(&file, box_side) {
            Ok(icon) => Some(icon),
            Err(err) => {
                tracing::debug!(app = %app.id, ?file, "unusable icon: {err:#}");
                None
            }
        });
    let icon = icon.unwrap_or_else(|| {
        let letter: String = app
            .name
            .chars()
            .take(1)
            .flat_map(char::to_uppercase)
            .collect();
        text(
            fonts,
            swash,
            &letter,
            Family::Name("Inter Display"),
            LETTER_SIZE,
            LETTER_COLOUR,
            size,
        )
    });
    let centre = |length: u32| (side - length as i32) / 2;
    pixmap.draw_pixmap(
        centre(icon.width()),
        centre(icon.height()),
        icon.as_ref(),
        &PixmapPaint::default(),
        tiny_skia::Transform::identity(),
        None,
    );
    pixmap
}

/// An icon fitted into a `side` square, keeping its aspect. SVGs are drawn
/// at that size, so they stay sharp at 4K; PNGs are resampled.
fn icon(file: &IconFile, side: f32) -> anyhow::Result<Pixmap> {
    let fit = |w: f32, h: f32| side / w.max(h);
    let (source, scale) = match file {
        IconFile::Svg(path) => {
            let tree = resvg::usvg::Tree::from_data(
                &std::fs::read(path)?,
                &resvg::usvg::Options::default(),
            )?;
            let size = tree.size();
            let scale = fit(size.width(), size.height());
            let mut pixmap = Pixmap::new(
                (size.width() * scale).ceil() as u32,
                (size.height() * scale).ceil() as u32,
            )
            .context("an empty SVG")?;
            resvg::render(
                &tree,
                tiny_skia::Transform::from_scale(scale, scale),
                &mut pixmap.as_mut(),
            );
            return Ok(pixmap);
        }
        IconFile::Png(path) => {
            let png = Pixmap::load_png(path)?;
            let scale = fit(png.width() as f32, png.height() as f32);
            (png, scale)
        }
    };
    let mut pixmap = Pixmap::new(
        (source.width() as f32 * scale).ceil() as u32,
        (source.height() as f32 * scale).ceil() as u32,
    )
    .context("an empty PNG")?;
    pixmap.draw_pixmap(
        0,
        0,
        source.as_ref(),
        &PixmapPaint {
            quality: FilterQuality::Bicubic,
            ..PixmapPaint::default()
        },
        tiny_skia::Transform::from_scale(scale, scale),
        None,
    );
    Ok(pixmap)
}

/// One line of text, cut off at `max_width`, in a pixmap just big enough
/// for it.
fn text(
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    text: &str,
    family: Family,
    size: f32,
    colour: Rgb,
    max_width: i32,
) -> Pixmap {
    let mut buffer = Buffer::new(fonts, Metrics::new(size, size * 1.25));
    buffer.set_wrap(Wrap::None);
    buffer.set_size(None, None);
    buffer.set_text(
        text,
        &Attrs::new().family(family).weight(Weight::EXTRA_BOLD),
        Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(fonts, false);
    let width = buffer
        .layout_runs()
        .map(|run| run.line_w)
        .fold(0.0, f32::max)
        .ceil() as i32;
    let height = (size * 1.25).ceil() as i32;
    let mut pixmap =
        Pixmap::new(width.clamp(1, max_width) as u32, height as u32).expect("a non-empty size");
    let Rgb([r, g, b]) = colour;
    buffer.draw(fonts, swash, Color::rgb(r, g, b), |x, y, w, h, colour| {
        blend(&mut pixmap, x, y, w, h, colour);
    });
    pixmap
}

/// Source-over of one solid rectangle, which is what cosmic-text draws
/// glyph coverage as.
fn blend(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, colour: Color) {
    let alpha = u32::from(colour.a());
    if alpha == 0 {
        return;
    }
    let (width, height) = (pixmap.width() as i32, pixmap.height() as i32);
    let premultiply = |c: u8| u32::from(c) * alpha / 255;
    let src = [
        premultiply(colour.r()),
        premultiply(colour.g()),
        premultiply(colour.b()),
        alpha,
    ];
    let pixels = pixmap.pixels_mut();
    for py in y.max(0)..(y + h as i32).min(height) {
        for px in x.max(0)..(x + w as i32).min(width) {
            let pixel = &mut pixels[(py * width + px) as usize];
            let dst = [pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()];
            let [r, g, b, a] =
                std::array::from_fn(|i| (src[i] + u32::from(dst[i]) * (255 - alpha) / 255) as u8);
            if let Some(blended) = PremultipliedColorU8::from_rgba(r, g, b, a) {
                *pixel = blended;
            }
        }
    }
}

fn rounded_rect(x: f32, y: f32, side: f32, radius: f32) -> Path {
    // How far along a corner's tangents a cubic's control points sit to
    // approximate a quarter circle.
    let k = radius * 0.552_284_8;
    let (right, bottom) = (x + side, y + side);
    let mut path = PathBuilder::new();
    path.move_to(x + radius, y);
    path.line_to(right - radius, y);
    path.cubic_to(
        right - radius + k,
        y,
        right,
        y + radius - k,
        right,
        y + radius,
    );
    path.line_to(right, bottom - radius);
    path.cubic_to(
        right,
        bottom - radius + k,
        right - radius + k,
        bottom,
        right - radius,
        bottom,
    );
    path.line_to(x + radius, bottom);
    path.cubic_to(
        x + radius - k,
        bottom,
        x,
        bottom - radius + k,
        x,
        bottom - radius,
    );
    path.line_to(x, y + radius);
    path.cubic_to(x, y + radius - k, x + radius - k, y, x + radius, y);
    path.close();
    path.finish().expect("a rounded rectangle is a valid path")
}

fn colour(Rgb([r, g, b]): Rgb) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(r, g, b, 255)
}

fn solid(rgb: Rgb) -> Paint<'static> {
    Paint {
        shader: Shader::SolidColor(colour(rgb)),
        anti_alias: true,
        ..Paint::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xs(count: usize, focus: usize) -> Vec<(usize, i32, bool)> {
        ribbon(count, focus)
            .map(|p| (p.index, p.x, p.focused))
            .collect()
    }

    #[test]
    fn focus_on_the_first_app_starts_the_row_at_the_left_margin() {
        assert_eq!(
            xs(37, 0),
            [
                (0, 192, true),
                (1, 808, false),
                (2, 1304, false),
                (3, 1800, false),
                (4, 2296, false),
                (5, 2792, false),
                (6, 3288, false),
                // Cut off by the screen's edge: there is more to the right.
                (7, 3784, false),
            ]
        );
    }

    #[test]
    fn moving_right_scrolls_once_four_apps_are_left_of_the_focus() {
        let row = xs(37, 10);
        assert_eq!(row[0], (6, 192, false));
        assert_eq!(row[4], (10, 2176, true));
        assert_eq!(row[5], (11, 2792, false));
    }

    #[test]
    fn a_short_row_ends_where_the_apps_do() {
        assert_eq!(xs(2, 1), [(0, 192, false), (1, 688, true)]);
        assert_eq!(xs(0, 0), []);
    }
}
