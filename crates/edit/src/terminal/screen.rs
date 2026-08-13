// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The screen a terminal application thinks it's drawing to.
//!
//! This is a plain grid of [`Cell`]s plus a cursor and the handful of modes
//! that change what writing to it does. [`super::emulator`] turns VT sequences
//! into calls on this type; nothing in here knows about VT.

use std::collections::VecDeque;

use crate::helpers::*;
use crate::unicode::char_width;

/// The default number of scrollback lines kept for the primary screen.
pub const DEFAULT_SCROLLBACK: usize = 10000;

/// A color as the terminal application asked for it.
///
/// Indexed colors stay unresolved until rendering, so that the panel picks up
/// the same palette the editor detected from its own terminal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Color(u32);

impl Color {
    const KIND_DEFAULT: u32 = 0 << 24;
    const KIND_INDEXED: u32 = 1 << 24;
    const KIND_RGB: u32 = 2 << 24;

    /// The terminal's default foreground/background, whichever applies.
    pub const DEFAULT: Self = Self(Self::KIND_DEFAULT);

    pub const fn indexed(index: u8) -> Self {
        Self(Self::KIND_INDEXED | index as u32)
    }

    pub const fn rgb(rgb: u32) -> Self {
        Self(Self::KIND_RGB | (rgb & 0xff_ffff))
    }

    pub const fn is_default(self) -> bool {
        self.0 & 0xff00_0000 == Self::KIND_DEFAULT
    }

    /// Resolves to a plain `0xRRGGBB`, given the palette's 16 base colors.
    ///
    /// Returns `None` for [`Color::DEFAULT`], which the caller has to map to
    /// its own default foreground or background.
    pub fn to_rgb(self, palette: &[u32; 16]) -> Option<u32> {
        match self.0 & 0xff00_0000 {
            Self::KIND_RGB => Some(self.0 & 0xff_ffff),
            Self::KIND_INDEXED => Some(indexed_to_rgb((self.0 & 0xff) as u8, palette)),
            _ => None,
        }
    }
}

/// Expands an xterm-256 index into an RGB value.
fn indexed_to_rgb(index: u8, palette: &[u32; 16]) -> u32 {
    match index {
        // The first 16 are whatever the palette says they are.
        0..=15 => palette[index as usize],
        // 6x6x6 color cube. The steps aren't linear; this is the xterm ramp.
        16..=231 => {
            const STEPS: [u32; 6] = [0, 95, 135, 175, 215, 255];
            let i = (index - 16) as usize;
            let r = STEPS[i / 36];
            let g = STEPS[(i / 6) % 6];
            let b = STEPS[i % 6];
            (r << 16) | (g << 8) | b
        }
        // 24 step grayscale ramp.
        _ => {
            let l = 8 + (index as u32 - 232) * 10;
            (l << 16) | (l << 8) | l
        }
    }
}

/// Per-cell rendition flags.
///
/// This doesn't reuse [`crate::framebuffer::Attributes`], because a terminal
/// needs reverse video and dim on top of what the editor's own renderer uses.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct CellAttributes(u8);

impl CellAttributes {
    pub const NONE: Self = Self(0);
    pub const BOLD: Self = Self(1 << 0);
    pub const ITALIC: Self = Self(1 << 1);
    pub const UNDERLINE: Self = Self(1 << 2);
    pub const STRIKETHROUGH: Self = Self(1 << 3);
    pub const REVERSE: Self = Self(1 << 4);
    pub const DIM: Self = Self(1 << 5);
    /// Marks the second cell of a double width character.
    /// Such a cell is never drawn; the wide character covers it.
    pub const WIDE_TRAILER: Self = Self(1 << 6);

    pub const fn has(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

/// One character cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub attr: CellAttributes,
}

impl Default for Cell {
    fn default() -> Self {
        Self { ch: ' ', fg: Color::DEFAULT, bg: Color::DEFAULT, attr: CellAttributes::NONE }
    }
}

impl Cell {
    /// A blank cell that still carries the current colors, which is what
    /// erasing has to leave behind so that a colored background survives.
    fn blank(pen: &Cell) -> Self {
        Self { ch: ' ', fg: pen.fg, bg: pen.bg, attr: pen.attr.without(CellAttributes::WIDE_TRAILER) }
    }
}

type Row = Vec<Cell>;

/// Which part of the screen an erase applies to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EraseScope {
    ToEnd,
    ToStart,
    All,
}

/// A grid plus its scrollback.
///
/// The visible screen is always the *last* `height` rows of `lines`; anything
/// before that is scrollback.
struct Grid {
    lines: VecDeque<Row>,
    width: CoordType,
    height: CoordType,
    scrollback_limit: usize,
}

impl Grid {
    fn new(size: Size, scrollback_limit: usize) -> Self {
        let width = size.width.max(1);
        let height = size.height.max(1);
        let mut lines = VecDeque::with_capacity(height as usize);
        for _ in 0..height {
            lines.push_back(vec![Cell::default(); width as usize]);
        }
        Self { lines, width, height, scrollback_limit }
    }

    /// Index into `lines` at which the visible screen starts.
    fn origin(&self) -> usize {
        self.lines.len() - self.height as usize
    }

    fn row_mut(&mut self, y: CoordType) -> &mut Row {
        let index = self.origin() + y as usize;
        &mut self.lines[index]
    }

    fn scrollback_len(&self) -> usize {
        self.lines.len() - self.height as usize
    }
}

/// The state of a terminal screen.
pub struct Screen {
    primary: Grid,
    alternate: Grid,
    on_alternate: bool,

    cursor: Point,
    saved_cursor: Point,
    /// The pen carries the colors and attributes new text is written with.
    pen: Cell,
    saved_pen: Cell,

    /// Inclusive top and exclusive bottom of the DECSTBM scroll region.
    scroll_top: CoordType,
    scroll_bottom: CoordType,

    /// VT wraps *lazily*: writing into the last column leaves the cursor there
    /// and only moves to the next line when another character arrives.
    /// Getting this wrong misaligns every box drawing TUI.
    wrap_pending: bool,
    autowrap: bool,

    pub cursor_visible: bool,
    pub cursor_style: CursorStyle,
    pub bracketed_paste: bool,
    pub application_cursor_keys: bool,
    /// Mode 2026. We always paint a whole frame at once, so honouring this is
    /// just a matter of admitting we support it.
    pub synchronized_output: bool,
    pub mouse_mode: MouseMode,
    pub title: String,

    /// How far back the user scrolled, in lines. 0 means "following the output".
    view_offset: usize,
    /// Bumped on every change so the UI knows whether it has to redraw.
    generation: u64,
}

/// The shape the application asked the cursor to take (DECSCUSR).
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum CursorStyle {
    #[default]
    Block,
    Underline,
    Bar,
}

/// Which mouse reports the application asked for.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum MouseMode {
    #[default]
    Off,
    /// Button presses and releases only (mode 1000).
    Buttons,
    /// Also motion while a button is held (mode 1002).
    Drag,
    /// Any motion (mode 1003).
    Motion,
}

impl Screen {
    pub fn new(size: Size, scrollback_limit: usize) -> Self {
        let size = Size { width: size.width.max(1), height: size.height.max(1) };
        Self {
            primary: Grid::new(size, scrollback_limit),
            alternate: Grid::new(size, 0),
            on_alternate: false,

            cursor: Point { x: 0, y: 0 },
            saved_cursor: Point { x: 0, y: 0 },
            pen: Cell::default(),
            saved_pen: Cell::default(),

            scroll_top: 0,
            scroll_bottom: size.height,

            wrap_pending: false,
            autowrap: true,

            cursor_visible: true,
            cursor_style: CursorStyle::Block,
            bracketed_paste: false,
            application_cursor_keys: false,
            synchronized_output: false,
            mouse_mode: MouseMode::Off,
            title: String::new(),

            view_offset: 0,
            generation: 1,
        }
    }

    fn grid(&self) -> &Grid {
        if self.on_alternate { &self.alternate } else { &self.primary }
    }

    fn grid_mut(&mut self) -> &mut Grid {
        if self.on_alternate { &mut self.alternate } else { &mut self.primary }
    }

    pub fn size(&self) -> Size {
        let g = self.grid();
        Size { width: g.width, height: g.height }
    }

    pub fn cursor(&self) -> Point {
        self.cursor
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn on_alternate(&self) -> bool {
        self.on_alternate
    }

    fn touch(&mut self) {
        self.generation += 1;
    }

    // ---- scrollback viewing ------------------------------------------------

    pub fn scrollback_len(&self) -> usize {
        self.grid().scrollback_len()
    }

    pub fn view_offset(&self) -> usize {
        self.view_offset
    }

    /// Scrolls the view by `delta` lines, positive meaning "towards history".
    pub fn scroll_view(&mut self, delta: CoordType) {
        let max = self.scrollback_len() as CoordType;
        let next = (self.view_offset as CoordType + delta).clamp(0, max) as usize;
        if next != self.view_offset {
            self.view_offset = next;
            self.touch();
        }
    }

    pub fn scroll_view_to_bottom(&mut self) {
        self.scroll_view(-(self.view_offset as CoordType));
    }

    /// Returns the row to display at screen line `y`, taking scrollback into account.
    pub fn visible_row(&self, y: CoordType) -> &[Cell] {
        let g = self.grid();
        let index = g.origin() + y as usize;
        // Scrolling back shifts the window towards the front of `lines`.
        let index = index.saturating_sub(self.view_offset);
        &g.lines[index]
    }

    // ---- geometry ----------------------------------------------------------

    pub fn resize(&mut self, size: Size) {
        let width = size.width.max(1);
        let height = size.height.max(1);
        if width == self.grid().width && height == self.grid().height {
            return;
        }

        for grid in [&mut self.primary, &mut self.alternate] {
            resize_grid(grid, width, height);
        }

        self.scroll_top = 0;
        self.scroll_bottom = height;
        self.cursor.x = self.cursor.x.min(width - 1);
        self.cursor.y = self.cursor.y.min(height - 1);
        self.wrap_pending = false;
        self.view_offset = 0;
        self.touch();
    }

    // ---- writing -----------------------------------------------------------

    /// Writes a printable character at the cursor and advances it.
    pub fn write_char(&mut self, ch: char) {
        let width = char_width(ch);

        // Combining marks attach to whatever came before instead of
        // consuming a cell of their own.
        if width == 0 {
            return;
        }

        let screen_width = self.grid().width;

        if self.wrap_pending && self.autowrap {
            self.carriage_return();
            self.line_feed();
        }
        self.wrap_pending = false;

        // A wide character doesn't fit in the last column, so it wraps early.
        if width == 2 && self.cursor.x == screen_width - 1 {
            if self.autowrap {
                self.carriage_return();
                self.line_feed();
            } else {
                // Nowhere to put it; leave the cell blank rather than draw half.
                return;
            }
        }

        let x = self.cursor.x;
        let y = self.cursor.y;
        let pen = self.pen;

        {
            let row = self.grid_mut().row_mut(y);
            row[x as usize] = Cell { ch, ..pen };
            if width == 2 {
                row[x as usize + 1] =
                    Cell { ch: ' ', attr: pen.attr.with(CellAttributes::WIDE_TRAILER), ..pen };
            }
        }

        self.cursor.x += width;
        if self.cursor.x >= screen_width {
            // Don't move to the next line yet -- see `wrap_pending`.
            self.cursor.x = screen_width - 1;
            self.wrap_pending = true;
        }

        self.follow_output();
        self.touch();
    }

    /// Any output from the application snaps the view back to the bottom,
    /// which is what every other terminal does.
    fn follow_output(&mut self) {
        self.view_offset = 0;
    }

    pub fn carriage_return(&mut self) {
        self.cursor.x = 0;
        self.wrap_pending = false;
        self.touch();
    }

    /// Moves down one line, scrolling the region if we're at its bottom.
    pub fn line_feed(&mut self) {
        self.wrap_pending = false;
        if self.cursor.y + 1 == self.scroll_bottom {
            self.scroll_up(1);
        } else if self.cursor.y + 1 < self.grid().height {
            self.cursor.y += 1;
        }
        self.follow_output();
        self.touch();
    }

    /// Moves up one line, scrolling the region if we're at its top (RI).
    pub fn reverse_index(&mut self) {
        self.wrap_pending = false;
        if self.cursor.y == self.scroll_top {
            self.scroll_down(1);
        } else if self.cursor.y > 0 {
            self.cursor.y -= 1;
        }
        self.touch();
    }

    pub fn tab(&mut self) {
        let width = self.grid().width;
        // Tab stops every 8 columns, which is the only thing anyone uses.
        let next = ((self.cursor.x / 8) + 1) * 8;
        self.cursor.x = next.min(width - 1);
        self.wrap_pending = false;
        self.touch();
    }

    pub fn backspace(&mut self) {
        if self.cursor.x > 0 {
            self.cursor.x -= 1;
        }
        self.wrap_pending = false;
        self.touch();
    }

    // ---- cursor ------------------------------------------------------------

    pub fn move_to(&mut self, x: CoordType, y: CoordType) {
        let size = self.size();
        self.cursor.x = x.clamp(0, size.width - 1);
        self.cursor.y = y.clamp(0, size.height - 1);
        self.wrap_pending = false;
        self.touch();
    }

    pub fn move_by(&mut self, dx: CoordType, dy: CoordType) {
        let x = self.cursor.x + dx;
        let y = self.cursor.y + dy;
        self.move_to(x, y);
    }

    pub fn save_cursor(&mut self) {
        self.saved_cursor = self.cursor;
        self.saved_pen = self.pen;
    }

    pub fn restore_cursor(&mut self) {
        let saved = self.saved_cursor;
        self.pen = self.saved_pen;
        self.move_to(saved.x, saved.y);
    }

    // ---- pen ---------------------------------------------------------------

    pub fn pen_mut(&mut self) -> &mut Cell {
        &mut self.pen
    }

    pub fn reset_pen(&mut self) {
        self.pen = Cell::default();
    }

    // ---- scrolling ---------------------------------------------------------

    pub fn set_scroll_region(&mut self, top: CoordType, bottom: CoordType) {
        let height = self.grid().height;
        let top = top.clamp(0, height - 1);
        let bottom = bottom.clamp(top + 1, height);
        self.scroll_top = top;
        self.scroll_bottom = bottom;
        self.touch();
    }

    pub fn scroll_region(&self) -> (CoordType, CoordType) {
        (self.scroll_top, self.scroll_bottom)
    }

    /// Scrolls the region up, i.e. content moves towards the top and blank
    /// lines appear at the bottom.
    pub fn scroll_up(&mut self, count: CoordType) {
        let count = count.max(0);
        if count == 0 {
            return;
        }

        let top = self.scroll_top;
        let bottom = self.scroll_bottom;
        let height = self.grid().height;
        let pen = self.pen;
        let keep_history = !self.on_alternate && top == 0 && bottom == height;

        for _ in 0..count.min(bottom - top) {
            if keep_history {
                // The line leaving the top of the screen becomes scrollback,
                // which is only meaningful when the region is the whole screen.
                let grid = self.grid_mut();
                let blank = vec![Cell::blank(&pen); grid.width as usize];
                grid.lines.push_back(blank);
                while grid.scrollback_len() > grid.scrollback_limit {
                    grid.lines.pop_front();
                }
            } else {
                let grid = self.grid_mut();
                let origin = grid.origin();
                let mut row = grid.lines.remove(origin + top as usize).unwrap();
                row.fill(Cell::blank(&pen));
                grid.lines.insert(origin + bottom as usize - 1, row);
            }
        }

        self.touch();
    }

    /// Scrolls the region down, i.e. blank lines appear at the top.
    pub fn scroll_down(&mut self, count: CoordType) {
        let count = count.max(0);
        let top = self.scroll_top;
        let bottom = self.scroll_bottom;
        let pen = self.pen;

        for _ in 0..count.min(bottom - top) {
            let grid = self.grid_mut();
            let origin = grid.origin();
            let mut row = grid.lines.remove(origin + bottom as usize - 1).unwrap();
            row.fill(Cell::blank(&pen));
            grid.lines.insert(origin + top as usize, row);
        }

        self.touch();
    }

    // ---- erasing and editing ----------------------------------------------

    pub fn erase_in_display(&mut self, scope: EraseScope) {
        let size = self.size();
        let cursor = self.cursor;
        let blank = Cell::blank(&self.pen);

        let (first, last) = match scope {
            EraseScope::ToEnd => (cursor.y, size.height - 1),
            EraseScope::ToStart => (0, cursor.y),
            EraseScope::All => (0, size.height - 1),
        };

        for y in first..=last {
            let partial_start = scope == EraseScope::ToEnd && y == cursor.y;
            let partial_end = scope == EraseScope::ToStart && y == cursor.y;
            let row = self.grid_mut().row_mut(y);
            let len = row.len();
            let from = if partial_start { cursor.x as usize } else { 0 };
            let to = if partial_end { cursor.x as usize + 1 } else { len };
            row[from.min(len)..to.min(len)].fill(blank);
        }

        self.touch();
    }

    pub fn erase_in_line(&mut self, scope: EraseScope) {
        let cursor = self.cursor;
        let blank = Cell::blank(&self.pen);
        let row = self.grid_mut().row_mut(cursor.y);
        let len = row.len();

        let (from, to) = match scope {
            EraseScope::ToEnd => (cursor.x as usize, len),
            EraseScope::ToStart => (0, cursor.x as usize + 1),
            EraseScope::All => (0, len),
        };
        row[from.min(len)..to.min(len)].fill(blank);

        self.touch();
    }

    /// Erases `count` cells starting at the cursor, without moving anything.
    pub fn erase_chars(&mut self, count: CoordType) {
        let cursor = self.cursor;
        let blank = Cell::blank(&self.pen);
        let row = self.grid_mut().row_mut(cursor.y);
        let from = cursor.x as usize;
        let to = (from + count.max(1) as usize).min(row.len());
        row[from..to].fill(blank);
        self.touch();
    }

    /// Inserts `count` blank cells at the cursor, pushing the rest right.
    pub fn insert_chars(&mut self, count: CoordType) {
        let cursor = self.cursor;
        let blank = Cell::blank(&self.pen);
        let row = self.grid_mut().row_mut(cursor.y);
        let at = cursor.x as usize;
        let count = (count.max(1) as usize).min(row.len() - at);
        row[at..].rotate_right(count);
        row[at..at + count].fill(blank);
        self.touch();
    }

    /// Deletes `count` cells at the cursor, pulling the rest left.
    pub fn delete_chars(&mut self, count: CoordType) {
        let cursor = self.cursor;
        let blank = Cell::blank(&self.pen);
        let row = self.grid_mut().row_mut(cursor.y);
        let at = cursor.x as usize;
        let count = (count.max(1) as usize).min(row.len() - at);
        row[at..].rotate_left(count);
        let from = row.len() - count;
        row[from..].fill(blank);
        self.touch();
    }

    /// Inserts `count` blank lines at the cursor, within the scroll region.
    pub fn insert_lines(&mut self, count: CoordType) {
        if self.cursor.y < self.scroll_top || self.cursor.y >= self.scroll_bottom {
            return;
        }
        // Scrolling down from the cursor row is exactly "insert blank lines".
        let saved_top = self.scroll_top;
        self.scroll_top = self.cursor.y;
        self.scroll_down(count.max(1));
        self.scroll_top = saved_top;
    }

    /// Deletes `count` lines at the cursor, within the scroll region.
    pub fn delete_lines(&mut self, count: CoordType) {
        if self.cursor.y < self.scroll_top || self.cursor.y >= self.scroll_bottom {
            return;
        }
        let saved_top = self.scroll_top;
        self.scroll_top = self.cursor.y;
        self.scroll_up(count.max(1));
        self.scroll_top = saved_top;
    }

    // ---- modes -------------------------------------------------------------

    pub fn set_autowrap(&mut self, on: bool) {
        self.autowrap = on;
    }

    pub fn autowrap(&self) -> bool {
        self.autowrap
    }

    /// Switches to or from the alternate screen (DECSET 1049).
    pub fn set_alternate(&mut self, on: bool) {
        if on == self.on_alternate {
            return;
        }

        if on {
            self.save_cursor();
            // Applications expect a clean slate on the alternate screen.
            let size = self.size();
            self.alternate = Grid::new(size, 0);
            self.on_alternate = true;
            self.scroll_top = 0;
            self.scroll_bottom = size.height;
            self.move_to(0, 0);
        } else {
            self.on_alternate = false;
            let size = self.size();
            self.scroll_top = 0;
            self.scroll_bottom = size.height;
            self.restore_cursor();
        }

        self.view_offset = 0;
        self.touch();
    }

    /// Full reset (RIS).
    pub fn reset(&mut self) {
        let size = self.size();
        let scrollback = self.primary.scrollback_limit;
        *self = Self::new(size, scrollback);
    }
}

/// Reflow-free resize: keep what fits, pad the rest.
///
/// Terminals disagree wildly on whether to rewrap long lines here, and not
/// rewrapping is both the simpler and the more predictable choice.
fn resize_grid(grid: &mut Grid, width: CoordType, height: CoordType) {
    if width != grid.width {
        for row in &mut grid.lines {
            row.resize(width as usize, Cell::default());
        }
        grid.width = width;
    }

    let old_height = grid.height as usize;
    let new_height = height as usize;

    if new_height > old_height {
        // Pull lines back out of the scrollback before inventing blank ones.
        let missing = new_height - old_height;
        let from_scrollback = grid.scrollback_len().min(missing);
        for _ in 0..(missing - from_scrollback) {
            grid.lines.push_back(vec![Cell::default(); width as usize]);
        }
    } else if new_height < old_height {
        // Push the extra lines into the scrollback instead of dropping them.
        let extra = old_height - new_height;
        if grid.scrollback_limit == 0 {
            for _ in 0..extra {
                grid.lines.pop_back();
            }
        }
    }

    grid.height = height;

    while grid.lines.len() < new_height {
        grid.lines.push_back(vec![Cell::default(); width as usize]);
    }
    while grid.scrollback_len() > grid.scrollback_limit {
        grid.lines.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: Size = Size { width: 10, height: 4 };

    fn screen() -> Screen {
        Screen::new(SIZE, 100)
    }

    fn write(screen: &mut Screen, text: &str) {
        for ch in text.chars() {
            screen.write_char(ch);
        }
    }

    fn line(screen: &Screen, y: CoordType) -> String {
        screen
            .visible_row(y)
            .iter()
            .filter(|c| !c.attr.has(CellAttributes::WIDE_TRAILER))
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn writes_and_wraps_lazily() {
        let mut s = screen();
        write(&mut s, "0123456789");

        // Filling the last column must NOT move to the next line yet.
        assert_eq!(s.cursor(), Point { x: 9, y: 0 });
        assert_eq!(line(&s, 0), "0123456789");

        // ...only the next character wraps.
        write(&mut s, "A");
        assert_eq!(s.cursor(), Point { x: 1, y: 1 });
        assert_eq!(line(&s, 1), "A");
    }

    #[test]
    fn honours_autowrap_off() {
        let mut s = screen();
        s.set_autowrap(false);
        write(&mut s, "0123456789ABC");
        assert_eq!(s.cursor().y, 0);
        // The last cell keeps being overwritten instead of wrapping.
        assert_eq!(line(&s, 0), "012345678C");
    }

    #[test]
    fn wide_characters_claim_two_cells() {
        let mut s = screen();
        write(&mut s, "ab漢");
        assert_eq!(s.cursor(), Point { x: 4, y: 0 });
        assert!(s.visible_row(0)[3].attr.has(CellAttributes::WIDE_TRAILER));
        assert_eq!(line(&s, 0), "ab漢");
    }

    #[test]
    fn wide_character_wraps_rather_than_splitting() {
        let mut s = screen();
        write(&mut s, "012345678");
        // Only one column left, which a wide character can't use.
        write(&mut s, "漢");
        assert_eq!(s.cursor(), Point { x: 2, y: 1 });
        assert_eq!(line(&s, 1), "漢");
    }

    #[test]
    fn combining_marks_do_not_advance() {
        let mut s = screen();
        write(&mut s, "a\u{0301}");
        assert_eq!(s.cursor(), Point { x: 1, y: 0 });
    }

    #[test]
    fn line_feed_at_the_bottom_scrolls_into_scrollback() {
        let mut s = screen();
        for i in 0..4 {
            write(&mut s, &format!("line{i}"));
            s.carriage_return();
            s.line_feed();
        }

        assert_eq!(s.scrollback_len(), 1);
        assert_eq!(line(&s, 0), "line1");
        assert_eq!(line(&s, 3), "");

        // Scrolling back reveals the line that left the screen.
        s.scroll_view(1);
        assert_eq!(line(&s, 0), "line0");
    }

    #[test]
    fn scroll_region_keeps_lines_outside_it() {
        let mut s = screen();
        for i in 0..4 {
            s.move_to(0, i);
            write(&mut s, &format!("l{i}"));
        }

        // Region covers rows 1..3, so rows 0 and 3 must not move.
        s.set_scroll_region(1, 3);
        s.move_to(0, 2);
        s.line_feed();

        assert_eq!(line(&s, 0), "l0");
        assert_eq!(line(&s, 1), "l2");
        assert_eq!(line(&s, 2), "");
        assert_eq!(line(&s, 3), "l3");
        // A partial region must never feed the scrollback.
        assert_eq!(s.scrollback_len(), 0);
    }

    #[test]
    fn insert_and_delete_lines_stay_in_the_region() {
        let mut s = screen();
        for i in 0..4 {
            s.move_to(0, i);
            write(&mut s, &format!("l{i}"));
        }

        s.set_scroll_region(1, 3);
        s.move_to(0, 1);
        s.insert_lines(1);

        assert_eq!(line(&s, 0), "l0");
        assert_eq!(line(&s, 1), "");
        assert_eq!(line(&s, 2), "l1");
        assert_eq!(line(&s, 3), "l3");

        s.delete_lines(1);
        assert_eq!(line(&s, 1), "l1");
        assert_eq!(line(&s, 2), "");
        assert_eq!(line(&s, 3), "l3");
    }

    #[test]
    fn insert_and_delete_chars() {
        let mut s = screen();
        write(&mut s, "abcdef");
        s.move_to(2, 0);
        s.insert_chars(2);
        assert_eq!(line(&s, 0), "ab  cdef");

        s.delete_chars(2);
        assert_eq!(line(&s, 0), "abcdef");
    }

    #[test]
    fn erase_respects_scope() {
        let mut s = screen();
        write(&mut s, "abcdef");
        s.move_to(3, 0);
        s.erase_in_line(EraseScope::ToEnd);
        assert_eq!(line(&s, 0), "abc");

        write(&mut s, "XYZ");
        s.move_to(4, 0);
        s.erase_in_line(EraseScope::ToStart);
        assert_eq!(line(&s, 0), "     Z");
    }

    #[test]
    fn alternate_screen_is_separate_and_restores() {
        let mut s = screen();
        write(&mut s, "primary");
        s.move_to(3, 1);

        s.set_alternate(true);
        assert_eq!(line(&s, 0), "");
        write(&mut s, "alt");
        assert_eq!(line(&s, 0), "alt");

        s.set_alternate(false);
        assert_eq!(line(&s, 0), "primary");
        assert_eq!(s.cursor(), Point { x: 3, y: 1 });
    }

    #[test]
    fn alternate_screen_has_no_scrollback() {
        let mut s = screen();
        s.set_alternate(true);
        for _ in 0..10 {
            s.line_feed();
        }
        assert_eq!(s.scrollback_len(), 0);
    }

    #[test]
    fn resize_keeps_content_and_clamps_the_cursor() {
        let mut s = screen();
        write(&mut s, "hello");
        s.move_to(9, 3);

        s.resize(Size { width: 5, height: 2 });
        assert_eq!(s.size(), Size { width: 5, height: 2 });
        assert_eq!(s.cursor(), Point { x: 4, y: 1 });
        // The scroll region has to follow the new height.
        assert_eq!(s.scroll_region(), (0, 2));
    }

    #[test]
    fn output_snaps_the_view_back_to_the_bottom() {
        let mut s = screen();
        for i in 0..8 {
            write(&mut s, &format!("l{i}"));
            s.carriage_return();
            s.line_feed();
        }

        s.scroll_view(3);
        assert_eq!(s.view_offset(), 3);

        write(&mut s, "x");
        assert_eq!(s.view_offset(), 0);
    }

    #[test]
    fn erasing_keeps_the_current_background() {
        let mut s = screen();
        s.pen_mut().bg = Color::indexed(4);
        s.erase_in_line(EraseScope::All);
        assert_eq!(s.visible_row(0)[0].bg, Color::indexed(4));
    }

    #[test]
    fn indexed_colors_expand_as_xterm_does() {
        let palette = [0u32; 16];
        // Start of the color cube is pure black.
        assert_eq!(Color::indexed(16).to_rgb(&palette), Some(0x000000));
        // End of the color cube is pure white.
        assert_eq!(Color::indexed(231).to_rgb(&palette), Some(0xffffff));
        // Grayscale ramp.
        assert_eq!(Color::indexed(232).to_rgb(&palette), Some(0x080808));
        assert_eq!(Color::rgb(0x123456).to_rgb(&palette), Some(0x123456));
        assert_eq!(Color::DEFAULT.to_rgb(&palette), None);
    }
}
