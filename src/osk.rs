//! The on-screen keyboard: Menu opens it over a web app, and what you pick
//! on it is typed into the app through the seat keyboard.

use std::ops::Range;

use anyhow::Context;
use cosmic_text::{Family, FontSystem, SwashCache, fontdb};
use evdev::KeyCode;
use smithay::backend::{
    input::KeyState,
    renderer::{
        element::{
            Kind,
            memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
        },
        gles::GlesRenderer,
    },
};
use tiny_skia::{FillRule, Pixmap, PixmapPaint, Transform};

use crate::{gamepad::Layer, home, lifecycle::Session, state::Emrakul};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

/// What the controller does to the keyboard while it is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Move(Dir),
    /// A: the focused key.
    Press,
    /// B, wherever the focus is.
    Backspace,
    /// Menu again.
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Nothing,
    Moved,
    /// Press and release this on the seat keyboard.
    Type(KeyCode),
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Type(KeyCode, &'static str),
    Close,
}

impl Key {
    pub fn label(self) -> &'static str {
        match self {
            Key::Type(_, label) => label,
            Key::Close => "Close",
        }
    }
}

pub const COLUMNS: usize = 10;

const fn t(code: KeyCode, label: &'static str) -> Key {
    Key::Type(code, label)
}

/// A key wider than one column fills several neighbouring cells, so moving
/// up or down keeps the column you are in.
pub const LAYOUT: [[Key; COLUMNS]; 5] = {
    use KeyCode as K;
    const SPACE: Key = t(K::KEY_SPACE, "Space");
    const BACKSPACE: Key = t(K::KEY_BACKSPACE, "Backspace");
    const ENTER: Key = t(K::KEY_ENTER, "Enter");
    const CLOSE: Key = Key::Close;
    [
        [
            t(K::KEY_1, "1"),
            t(K::KEY_2, "2"),
            t(K::KEY_3, "3"),
            t(K::KEY_4, "4"),
            t(K::KEY_5, "5"),
            t(K::KEY_6, "6"),
            t(K::KEY_7, "7"),
            t(K::KEY_8, "8"),
            t(K::KEY_9, "9"),
            t(K::KEY_0, "0"),
        ],
        [
            t(K::KEY_Q, "q"),
            t(K::KEY_W, "w"),
            t(K::KEY_E, "e"),
            t(K::KEY_R, "r"),
            t(K::KEY_T, "t"),
            t(K::KEY_Y, "y"),
            t(K::KEY_U, "u"),
            t(K::KEY_I, "i"),
            t(K::KEY_O, "o"),
            t(K::KEY_P, "p"),
        ],
        [
            t(K::KEY_A, "a"),
            t(K::KEY_S, "s"),
            t(K::KEY_D, "d"),
            t(K::KEY_F, "f"),
            t(K::KEY_G, "g"),
            t(K::KEY_H, "h"),
            t(K::KEY_J, "j"),
            t(K::KEY_K, "k"),
            t(K::KEY_L, "l"),
            t(K::KEY_DOT, "."),
        ],
        [
            t(K::KEY_Z, "z"),
            t(K::KEY_X, "x"),
            t(K::KEY_C, "c"),
            t(K::KEY_V, "v"),
            t(K::KEY_B, "b"),
            t(K::KEY_N, "n"),
            t(K::KEY_M, "m"),
            BACKSPACE,
            BACKSPACE,
            BACKSPACE,
        ],
        [
            CLOSE, CLOSE, SPACE, SPACE, SPACE, SPACE, SPACE, ENTER, ENTER, ENTER,
        ],
    ]
};

/// The keys of one row as drawn: each key once, with the columns it spans.
pub fn spans(row: usize) -> impl Iterator<Item = (Key, Range<usize>)> {
    let keys = &LAYOUT[row];
    (0..COLUMNS)
        .filter(move |&col| col == 0 || keys[col - 1] != keys[col])
        .map(move |start| {
            let end = (start..COLUMNS)
                .find(|&col| keys[col] != keys[start])
                .unwrap_or(COLUMNS);
            (keys[start], start..end)
        })
}

/// Where the focus is: a cell of [`LAYOUT`], always in range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cell {
    row: usize,
    col: usize,
}

impl Cell {
    pub fn row(self) -> usize {
        self.row
    }

    pub fn key(self) -> Key {
        LAYOUT[self.row][self.col]
    }

    /// The columns the focused key covers.
    pub fn span(self) -> Range<usize> {
        spans(self.row)
            .map(|(_, cols)| cols)
            .find(|cols| cols.contains(&self.col))
            .expect("every column is in a span")
    }
}

#[derive(Debug, Clone, Copy)]
pub struct OnScreenKeyboard {
    pub focus: Cell,
}

impl OnScreenKeyboard {
    /// Opens with the focus on q, the top-left letter.
    pub fn new() -> Self {
        Self {
            focus: Cell { row: 1, col: 0 },
        }
    }

    pub fn on_input(&mut self, input: Input) -> Outcome {
        match input {
            Input::Press => match self.focus.key() {
                Key::Type(code, _) => Outcome::Type(code),
                Key::Close => Outcome::Close,
            },
            Input::Backspace => Outcome::Type(KeyCode::KEY_BACKSPACE),
            Input::Close => Outcome::Close,
            Input::Move(dir) => {
                let Cell { row, col } = self.focus;
                let span = self.focus.span();
                let to = match dir {
                    Dir::Up => row.checked_sub(1).map(|row| Cell { row, col }),
                    Dir::Down => (row + 1 < LAYOUT.len()).then_some(Cell { row: row + 1, col }),
                    // Off the whole key, not just the cell, and onto the
                    // first cell of the next one.
                    Dir::Left => span.start.checked_sub(1).map(|left| Cell {
                        row,
                        col: Cell { row, col: left }.span().start,
                    }),
                    Dir::Right => (span.end < COLUMNS).then_some(Cell { row, col: span.end }),
                };
                match to {
                    Some(to) => {
                        self.focus = to;
                        Outcome::Moved
                    }
                    None => Outcome::Nothing,
                }
            }
        }
    }
}

impl Emrakul {
    /// What the controller drives right now.
    pub fn pad_layer(&self) -> Layer {
        match self.session {
            Session::Running(_, Some(_)) => Layer::Keyboard,
            _ => Layer::App,
        }
    }

    /// Opens the keyboard over the running app. Home has no text to type.
    pub fn open_keyboard(&mut self) {
        if let Session::Running(_, keyboard @ None) = &mut self.session {
            *keyboard = Some(OnScreenKeyboard::new());
            self.backend.request_redraw(&self.loop_handle);
        }
    }

    pub fn on_keyboard_input(&mut self, input: Input) {
        let Session::Running(_, Some(keyboard)) = &mut self.session else {
            return;
        };
        match keyboard.on_input(input) {
            Outcome::Nothing => return,
            Outcome::Moved => {}
            Outcome::Type(code) => {
                self.pad_key(code, KeyState::Pressed);
                self.pad_key(code, KeyState::Released);
                return;
            }
            Outcome::Close => {
                if let Session::Running(_, keyboard) = &mut self.session {
                    *keyboard = None;
                }
            }
        }
        self.backend.request_redraw(&self.loop_handle);
    }
}

/// Laid out like Home, in the TV's physical pixels at 3840x2160.
const KEY_W: i32 = 232;
const KEY_H: i32 = 168;
const GAP: i32 = 20;
const PADDING: i32 = 48;
const KEY_RADIUS: f32 = 24.0;
const LABEL_SIZE: f32 = 80.0;
const PANEL_W: i32 = COLUMNS as i32 * (KEY_W + GAP) - GAP + 2 * PADDING;
const PANEL_H: i32 = LAYOUT.len() as i32 * (KEY_H + GAP) - GAP + 2 * PADDING;
/// Centred, this far above the bottom edge.
const PANEL_X: i32 = (home::SCREEN_W - PANEL_W) / 2;
const PANEL_Y: i32 = home::SCREEN_H - PANEL_H - 96;

/// Where a key spanning `cols` of `row` sits within the panel.
fn key_rect(row: usize, cols: &Range<usize>) -> (i32, i32, i32, i32) {
    let x = PADDING + cols.start as i32 * (KEY_W + GAP);
    let y = PADDING + row as i32 * (KEY_H + GAP);
    let w = cols.len() as i32 * (KEY_W + GAP) - GAP;
    (x, y, w, KEY_H)
}

type Element = MemoryRenderBufferRenderElement<GlesRenderer>;

/// The keyboard as drawn: the panel with every key, rasterised the first
/// time it opens, and the focused key over it, redrawn as the focus moves.
pub struct View {
    fonts: FontSystem,
    swash: SwashCache,
    panel: Option<MemoryRenderBuffer>,
    focused: Option<((usize, usize), MemoryRenderBuffer)>,
}

impl View {
    pub fn new() -> anyhow::Result<Self> {
        let mut db = fontdb::Database::new();
        db.load_font_file(home::FONT)
            .with_context(|| format!("loading {}", home::FONT))?;
        Ok(Self {
            fonts: FontSystem::new_with_locale_and_db("en-US".into(), db),
            swash: SwashCache::new(),
            panel: None,
            focused: None,
        })
    }

    /// The keyboard's elements, front to back.
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        keyboard: &OnScreenKeyboard,
    ) -> Vec<Element> {
        let row = keyboard.focus.row();
        let span = keyboard.focus.span();
        if self
            .focused
            .as_ref()
            .is_none_or(|(at, _)| *at != (row, span.start))
        {
            let (_, _, w, h) = key_rect(row, &span);
            let mut pixmap = Pixmap::new(w as u32, h as u32).expect("a non-empty size");
            self.draw_key(&mut pixmap, (0, 0, w, h), keyboard.focus.key(), true);
            self.focused = Some(((row, span.start), home::buffer(&pixmap, false)));
        }
        if self.panel.is_none() {
            let mut pixmap = Pixmap::new(PANEL_W as u32, PANEL_H as u32).expect("a non-empty size");
            pixmap.fill_path(
                &home::rounded_rect(0.0, 0.0, PANEL_W as f32, PANEL_H as f32, home::RADIUS),
                &home::solid(home::BACKGROUND),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
            for row in 0..LAYOUT.len() {
                for (key, cols) in spans(row) {
                    self.draw_key(&mut pixmap, key_rect(row, &cols), key, false);
                }
            }
            self.panel = Some(home::buffer(&pixmap, false));
        }

        let (x, y, _, _) = key_rect(row, &span);
        [
            (
                self.focused.as_ref().map(|(_, b)| b),
                PANEL_X + x,
                PANEL_Y + y,
            ),
            (self.panel.as_ref(), PANEL_X, PANEL_Y),
        ]
        .into_iter()
        .filter_map(|(buffer, x, y)| {
            Element::from_buffer(
                renderer,
                (f64::from(x), f64::from(y)),
                buffer?,
                None,
                None,
                None,
                Kind::Unspecified,
            )
            .inspect_err(|err| tracing::warn!(?err, "uploading the keyboard"))
            .ok()
        })
        .collect()
    }

    /// A key at `(x, y, w, h)` in `pixmap`: dark with a light label, or
    /// white with a dark one when focused, as Home's focus ring is white.
    fn draw_key(
        &mut self,
        pixmap: &mut Pixmap,
        (x, y, w, h): (i32, i32, i32, i32),
        key: Key,
        focused: bool,
    ) {
        let (fill, ink) = if focused {
            (home::RING_COLOUR, home::BACKGROUND)
        } else {
            (home::TILE_COLOUR, home::TITLE_COLOUR)
        };
        pixmap.fill_path(
            &home::rounded_rect(x as f32, y as f32, w as f32, h as f32, KEY_RADIUS),
            &home::solid(fill),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
        let label = home::text(
            &mut self.fonts,
            &mut self.swash,
            key.label(),
            Family::Name("Inter Display"),
            LABEL_SIZE,
            ink,
            w,
        );
        pixmap.draw_pixmap(
            x + (w - label.width() as i32) / 2,
            y + (h - label.height() as i32) / 2,
            label.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(keyboard: &mut OnScreenKeyboard, moves: &[Dir]) {
        for dir in moves {
            keyboard.on_input(Input::Move(*dir));
        }
    }

    fn typed(keyboard: &mut OnScreenKeyboard) -> Outcome {
        keyboard.on_input(Input::Press)
    }

    #[test]
    fn opens_on_q() {
        assert_eq!(
            typed(&mut OnScreenKeyboard::new()),
            Outcome::Type(KeyCode::KEY_Q)
        );
    }

    #[test]
    fn the_d_pad_walks_the_grid() {
        let mut keyboard = OnScreenKeyboard::new();
        walk(&mut keyboard, &[Dir::Right, Dir::Right, Dir::Down]);
        assert_eq!(typed(&mut keyboard), Outcome::Type(KeyCode::KEY_D));
        walk(&mut keyboard, &[Dir::Up, Dir::Up]);
        assert_eq!(typed(&mut keyboard), Outcome::Type(KeyCode::KEY_3));
    }

    #[test]
    fn the_edges_stop_the_focus() {
        let mut keyboard = OnScreenKeyboard::new();
        assert_eq!(keyboard.on_input(Input::Move(Dir::Left)), Outcome::Nothing);
        walk(&mut keyboard, &[Dir::Up]);
        assert_eq!(keyboard.on_input(Input::Move(Dir::Up)), Outcome::Nothing);
        assert_eq!(typed(&mut keyboard), Outcome::Type(KeyCode::KEY_1));
    }

    #[test]
    fn a_wide_key_is_one_step_across() {
        let mut keyboard = OnScreenKeyboard::new();
        // Down to the bottom row: Close.
        walk(&mut keyboard, &[Dir::Down, Dir::Down, Dir::Down]);
        assert_eq!(typed(&mut keyboard), Outcome::Close);
        walk(&mut keyboard, &[Dir::Right]);
        assert_eq!(typed(&mut keyboard), Outcome::Type(KeyCode::KEY_SPACE));
        walk(&mut keyboard, &[Dir::Right]);
        assert_eq!(typed(&mut keyboard), Outcome::Type(KeyCode::KEY_ENTER));
        assert_eq!(keyboard.on_input(Input::Move(Dir::Right)), Outcome::Nothing);
        walk(&mut keyboard, &[Dir::Left]);
        assert_eq!(typed(&mut keyboard), Outcome::Type(KeyCode::KEY_SPACE));
        walk(&mut keyboard, &[Dir::Left]);
        assert_eq!(typed(&mut keyboard), Outcome::Close);
    }

    #[test]
    fn up_from_a_wide_key_lands_above_where_you_entered_it() {
        let mut keyboard = OnScreenKeyboard::new();
        // From m (column 6) down onto Space, then back up: m again.
        walk(&mut keyboard, &[Dir::Down, Dir::Down]);
        walk(&mut keyboard, &[Dir::Right; 6]);
        assert_eq!(typed(&mut keyboard), Outcome::Type(KeyCode::KEY_M));
        walk(&mut keyboard, &[Dir::Down]);
        assert_eq!(typed(&mut keyboard), Outcome::Type(KeyCode::KEY_SPACE));
        walk(&mut keyboard, &[Dir::Up]);
        assert_eq!(typed(&mut keyboard), Outcome::Type(KeyCode::KEY_M));
        // Right from m onto Backspace, and left off it back onto m.
        walk(&mut keyboard, &[Dir::Right]);
        assert_eq!(typed(&mut keyboard), Outcome::Type(KeyCode::KEY_BACKSPACE));
        walk(&mut keyboard, &[Dir::Left]);
        assert_eq!(typed(&mut keyboard), Outcome::Type(KeyCode::KEY_M));
    }

    #[test]
    fn b_is_backspace_and_menu_closes_wherever_the_focus_is() {
        let mut keyboard = OnScreenKeyboard::new();
        assert_eq!(
            keyboard.on_input(Input::Backspace),
            Outcome::Type(KeyCode::KEY_BACKSPACE)
        );
        assert_eq!(keyboard.on_input(Input::Close), Outcome::Close);
    }

    #[test]
    fn every_row_spans_all_columns() {
        for row in 0..LAYOUT.len() {
            let cols: Vec<_> = spans(row).flat_map(|(_, cols)| cols).collect();
            assert_eq!(cols, (0..COLUMNS).collect::<Vec<_>>(), "row {row}");
        }
        assert_eq!(spans(4).count(), 3);
    }
}
