# How should the compositor draw its own UI?

Research for [#6](https://github.com/cramt/emrakul/issues/6). Covers Home (app list with icons, labels and a focus highlight, 3840x2160), plus the later toasts and OSD.

## Answer

Shape and rasterise text on the CPU with **cosmic-text** (it uses swash). Put each label and icon in its own **`MemoryRenderBuffer`**, render that once, and keep it. Draw the focus highlight and the backgrounds on the GPU. Use a `SolidColorRenderElement` for now and a `PixelShaderElement` once the corners need rounding. Combine these element types with the client surfaces in one `render_elements!` enum.

Skip toolkits. Both reference compositors use this shape. niri rasterises text on the CPU into textures and draws geometry with shaders. cosmic-comp does the same, but it puts iced on top of the CPU step, which costs it about 270 extra crates. Home is one list, and that is not enough UI to justify a toolkit.

## Where it plugs in

emrakul renders a single element type today: `type Element = WaylandSurfaceRenderElement<GlesRenderer>` (`src/drm.rs:66`). It builds a `Vec<Element>` from the space and passes it to `DrmOutput::render_frame` (`src/drm.rs:456-495`). The same type parameter is also given to `initialize_output::<_, Element>` (`src/drm.rs:377`).

Every option below means swapping that alias for a smithay `render_elements!` enum. Here is a sketch, using the variant syntax documented at `smithay/src/backend/renderer/element/mod.rs:1603-1676`:

```rust
render_elements! {
    pub Element<=GlesRenderer>;
    Surface = WaylandSurfaceRenderElement<GlesRenderer>,
    Memory  = MemoryRenderBufferRenderElement<GlesRenderer>,
    Solid   = SolidColorRenderElement,
    Shader  = PixelShaderElement,
}
```

Elements are listed front to back, so overlay UI goes before the surfaces. The order matters for scanout:

- A Home frame always goes through composition, and that costs nothing extra because it has no client to scan out.
- A toast or OSD shown over a fullscreen app also forces composition while it is visible. The reason is that `MemoryRenderBufferRenderElement` has no dmabuf behind it, so the DRM compositor cannot put it on a plane.
- Once the toast is gone, the client's `Kind::ScanoutCandidate` (`src/drm.rs:485`) gets direct scanout back.

## The smithay building blocks (rev `ff5fa7df`)

- **`MemoryRenderBuffer`** (`element/memory.rs`). This is a CPU pixel buffer with damage tracking built in.
  - Drawing goes through `buffer.render().draw(|pixels| ... Ok(damage_rects))`.
  - At render time, only the damage recorded since the texture's last commit is re-uploaded through `update_memory`. The texture itself is created once with `import_memory` (`memory.rs:313-332`).
  - The module docs describe it as the type "targeted at software rendering" (`memory.rs:1-16`).
- **`TextureBuffer` / `TextureRenderElement`** (`element/texture.rs`). Use these for static textures, or for textures you render into on the GPU with `TextureRenderBuffer`. The id must stay stable, or damage tracking breaks (`texture.rs:30-37`).
- **`SolidColorBuffer` / `SolidColorRenderElement`** (`element/solid.rs`). A coloured rectangle that the GPU fills directly, with nothing uploaded.
- **`PixelShaderElement`** (`gles/element.rs:13-60`) together with `GlesRenderer::compile_custom_pixel_shader` (`gles/mod.rs:1966`).
  - You write a GLSL ES 1.00 fragment shader. It receives `v_coords`, `size` and `alpha`, plus any uniforms you add.
  - `resize()` bumps the commit counter only when the area changes. This makes a rounded focus ring about 30 lines of GLSL.
- **`GlowRenderer`** (`renderer/glow.rs:41`). It wraps `GlesRenderer` and exposes a `glow::Context`. It is the only way to drive an external GL library like femtovg from inside smithay's frame.

## What niri does

niri 26.04 draws the hotkey overlay, screenshot UI, config-error notification, exit dialog and MRU titles with **pango + pangocairo** (`Cargo.toml:84-85`).

How it draws text:

1. It measures with a 0x0 cairo `ImageSurface`, renders into an `ARgb32` surface, and calls `take_data()` (`src/ui/config_error_notification.rs:199-231`, `src/ui/mru.rs:1665-1699`).
2. It uploads the result once with `import_memory` into its own copy of `TextureBuffer`, which adds fractional scale (`src/render_helpers/texture.rs:11-72`).
3. It wraps that in `PrimaryGpuTextureRenderElement` (`primary_gpu_texture.rs:12`).

How it caches. Each texture is cached per scale or per output, and rebuilt only when the content changes:

- MRU: `TitleTexture {title, scale, texture}` (`src/ui/mru.rs:190-196`).
- Hotkey overlay: a map per output, cleared on config change (`src/ui/hotkey_overlay.rs:34,75,95-101`).

There is no partial damage. New content means a new texture with a new `Id`, which the damage tracker treats as full damage for that element. That is fine for small, rarely changing text.

How it scales text. Font size is set in physical pixels (`set_absolute_size(to_physical_precise_round(scale, size))`, `config_error_notification.rs:142`).

How it draws geometry. Selection boxes and dimming use plain `SolidColorRenderElement`s (`src/ui/screenshot_ui.rs:586-616`). Borders, shadows and rounded corners use niri's own `ShaderRenderElement` stack (`render_helpers/shader_element.rs`, `border.rs`, `shadow.rs`, about 1,900 LOC with the `.frag` files). Most of that stack exists to support user-configurable window decorations. emrakul does not need it.

Size: the text-to-texture core (`texture.rs`, `primary_gpu_texture.rs`, `memory.rs`) is about 440 LOC.

What pango costs: system pango, cairo, glib/gobject, harfbuzz and fontconfig/freetype, all through `-sys` crates. `pangocairo` alone resolves to 61 crates (measured, below).

## What cosmic-comp does

cosmic-comp draws its UI with **iced through libcosmic, rendered in software by `iced_tiny_skia`**. This covers headers/SSD, stack tabs, resize and swap indicators, the context menu and zoom (`src/utils/iced/mod.rs:77`, used from `src/shell/element/window.rs:728`, `stack.rs:1063` and others).

The pipeline:

- Each `IcedElement` keeps one `MemoryRenderBuffer` per output scale (`mod.rs:181`).
- On redraw it diffs the new iced layers against the cached ones with `iced_graphics::damage::diff` (`mod.rs:978-1006`). tiny-skia then repaints only those rects (`mod.rs:1014`), and the same rects are passed to `MemoryRenderBuffer`, so only they get re-uploaded.
- The output is a `MemoryRenderBufferRenderElement` (`mod.rs:1051`).
- Text goes through cosmic-text 0.19 and swash. Fonts come from `fontdb` with the pure-Rust `fontconfig-parser`, so libfontconfig is not linked.

The costs:

- Each element gets its own calloop futures executor (`mod.rs:208-215`).
- The `Send`/`Sync` impls are commented as unsound (`mod.rs:85-86`).
- The glue is 1,342 LOC (`mod.rs` 1118 plus `state.rs` 224).
- Of about 588 resolved normal dependencies, about 273 come from libcosmic/iced and are not shared with smithay.

Geometric decorations bypass iced. Rounded outlines, rectangles, shadows and blur are 7 small fragment shaders (494 lines) compiled with `compile_custom_pixel_shader` and emitted as `PixelShaderElement`s (`src/backend/render/mod.rs:391-435,275,365`).

So the best-funded Smithay compositor still settles on the same split: text and widgets on the CPU into a `MemoryRenderBuffer`, shapes in shaders. iced only adds layout and widgets on top.

## Options compared

Dependency counts are the deduplicated `cargo tree -e normal,build` of a scratch crate that depends only on that crate. "New" means not already in emrakul's `Cargo.lock` (160 packages).

| Option | Render element | 4K text quality | Cost (i5-8300H + GTX 1050 Ti) | Damage fit | Deps | Code |
|---|---|---|---|---|---|---|
| **cosmic-text (swash) into `MemoryRenderBuffer` per label/icon, shapes as Solid/PixelShader** | `MemoryRenderBufferRenderElement`, `SolidColorRenderElement`, `PixelShaderElement` | Full shaping (harfrust), bidi, fallback, grayscale AA. Same text stack as cosmic-comp. | Rasterises once per label. A focus move uploads nothing and damages only the old and new ring rects. | Exact. Each label is its own element with a stable id, and a changed label re-uploads only itself. | 43 (cosmic-text no-default + fontconfig feature). With `png`: 48 total, **33 new**. Pure Rust, no system libs. | About 300-500 LOC (a niri-sized text helper plus layout for one list) |
| fontdue / ab_glyph instead of cosmic-text | same | No shaping. The fontdue README says it belongs to the class of libraries "that don't tackle shaping". No ligatures or complex scripts, and kerning only through its basic layout. Latin labels look fine. | Same as above | Same | 11 / 6 | Similar, plus your own line breaking and fallback |
| pango + pangocairo (niri) | Texture/Memory element | Best-in-class: hinting and fontconfig rendering rules. At 4K, 64-96 px text viewed from a sofa shows no difference from swash grayscale AA. | Same caching model | Same | 61 crates **plus system pango, cairo, glib, harfbuzz, freetype, fontconfig** in `buildInputs` | About 400 LOC |
| tiny-skia (alone, for vector icons and shapes on the CPU) | `MemoryRenderBufferRenderElement` | No text of its own (pair it with cosmic-text) | Full-surface CPU fills at 4K cost more than the GPU doing them | Good if limited to small buffers | 9 (no-default with simd) to 21 | Worth adding only if CPU-drawn vector shapes are needed. The GPU shader does rounded rects better. |
| femtovg (GL vector) | Custom `RenderElement` drawing through `GlowRenderer`'s `glow::Context` | Its own glyph atlas, shaping through rustybuzz | All on the GPU, no uploads | Poor. femtovg redraws everything it is given each flush and shares GL state with smithay's renderer, so you have to save and restore state and map damage yourself. | 40, plus switching `GlesRenderer` to `GlowRenderer` | Several hundred LOC of glue. README: "Rendering is done via one OpenGl (ES) 3.0+ backend". smithay's configless context asks for `CONTEXT_CLIENT_VERSION 2` (`egl/context.rs:268-271`), so 3.0 depends on what the driver hands back. |
| iced + iced_tiny_skia offscreen (cosmic-comp) | `MemoryRenderBufferRenderElement` | Same text stack as option 1 | Software raster of every damaged widget rect | Good (layer diffing, `mod.rs:978-1014`) | `iced_tiny_skia` alone is 113. cosmic-comp's full libcosmic adds about 273. | About 1,300 LOC of glue in cosmic-comp, plus an executor per element and unsound `Send`/`Sync` |
| Slint, software renderer | `MemoryRenderBufferRenderElement` (Slint's `SoftwareRenderer` renders into a buffer you provide, with dirty-region tracking) | Good | Moderate | Good (dirty regions) | 147 normal deps, 231 with build deps (its `.slint` compiler) | A DSL plus a platform-backend shim. **Licence:** the royalty-free licence "does not permit the use of the Software within Embedded Systems". A dedicated TV box arguably is one, so in practice that means GPLv3 or paid. |

### Measured: CPU text cost

I built a scratch crate against cosmic-text 0.19 (no default features, `swash` + `fontconfig`), release build, on luna (i7-4770K, Haswell). The i5-8300H's single-thread speed is in the same range. The test shapes and rasterises 12 Home-style labels ("YouTube", "Hollow Knight: Silksong", ...) into 1200xN RGBA buffers:

| Size | Cold (first time) | Warm (glyph cache hot) | of which shaping |
|---|---|---|---|
| 64 px | 19.8 ms | 3.3 ms | 0.6 ms |
| 96 px | 14.3 ms | 5.9 ms | 0.7 ms |

`FontSystem::new()` (scanning the system fonts, 159 faces) took 21-37 ms, once at startup.

Most of the warm time is spent in the pixel blit. `Buffer::draw`'s per-pixel callback took about twice as long as blitting `SwashCache::get_image` masks directly (5.4 ms against 2.0 ms at 64 px), so use the mask path.

Either way, a full relabel of Home takes less than one 60 Hz frame and happens only when the app list changes. Moving focus never re-rasterises.

### Unmeasured: GPU upload

A 1200x96 label is 460 KB, and a Home's worth of labels is about 5.5 MB, uploaded once. One full 3840x2160 ARGB buffer is 33 MB. That is a strong reason not to draw Home as one screen-sized CPU canvas: any change would either re-upload most of that or need careful sub-rect damage. Per-element buffers make the damage tracker do this work for free. I did not benchmark uploads on the 1050 Ti. Treat the sizes as the argument, not a timing.

## Recommendation

**cosmic-text + `MemoryRenderBuffer` per label and icon, with focus and backgrounds as `SolidColorRenderElement` (and later `PixelShaderElement` for rounded corners).**

The reasons:

1. **Small and monolithic.** It adds 33 new pure-Rust crates (including `png` for icons) and no system libraries. The flake's `buildInputs` stay as they are. pango would add six C libraries, and iced or Slint would add 100-270 crates for a single list.
2. **Damage comes for free.** Each label, icon and the focus ring is its own element with a stable `Id`. Moving focus re-renders two small rects on the GPU and uploads nothing. A changed label re-uploads one small buffer. An idle Home stays at zero cost, which fits the `Redraw` state machine (`src/drm.rs`).
3. **Good 10-foot text.** It is the same shaping and rasterisation stack cosmic-comp ships: full OpenType shaping, fallback, bidi. Hinting and subpixel AA, the places where pango is ahead, do not matter for 64-96 px glyphs on a 4K TV.
4. **It is what the reference compositors do once you remove their extras.** niri: CPU text into textures, cached per content and scale. cosmic-comp: `MemoryRenderBuffer` for text, `PixelShaderElement` for shapes.

What follows from this:

- **Icons.** Desktop-entry icons are PNG or SVG. Use `png` (11 crates). Add `resvg` with `default-features = false` (33 crates) only if SVG-only icons show up. Decode each icon once into a `MemoryRenderBuffer` at its display size. Icons that never change could go into a `TextureBuffer` instead.
- **Scale.** Run Home at scale 1.0 on the 4K output, as `render()` already does (`Scale::from(1.0)`), and size fonts in physical pixels the way niri does. That avoids fractional-scale caches entirely.
- **Revisit when** Home grows real widgets: text input, scrolling lists with clipping, settings forms. At that point iced on tiny-skia is the proven upgrade path, and it would reuse the same cosmic-text stack and the same `MemoryRenderBuffer` integration, so nothing from this approach gets thrown away.

## Sources

- emrakul `src/drm.rs` at `origin/main` (`7f3231b`).
- smithay at `ff5fa7df392cecfba049ffed55cdaa4e98a8e7ef`: `src/backend/renderer/element/{memory,texture,solid,mod}.rs`, `src/backend/renderer/gles/{element,mod}.rs`, `src/backend/renderer/glow.rs`, `src/backend/egl/context.rs`.
- niri 26.04 source: `Cargo.toml`, `src/ui/*.rs`, `src/render_helpers/*.rs`.
- cosmic-comp (local checkout): `Cargo.toml`, `src/utils/iced/{mod,state}.rs`, `src/backend/render/mod.rs`, `src/backend/render/shaders/`.
- Crate sources from crates.io: cosmic-text 0.19.0 (`Cargo.toml` features, `src/swash.rs`, `src/buffer.rs`), fontdue 0.9.4 README, femtovg 0.19.3 README, slint 1.18.1 `LICENSES/LicenseRef-Slint-Royalty-free-2.0.md`.
- Dependency counts: `cargo tree -e normal,build --prefix none`, deduplicated, one scratch crate per candidate, resolved 2026-10-02.
