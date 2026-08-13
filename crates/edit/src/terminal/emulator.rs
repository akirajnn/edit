// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Turns the VT stream a child process produces into [`Screen`] operations.
//!
//! The tokenizer is [`crate::vt`], the same one the editor uses to read its own
//! terminal's replies. All that's left here is deciding what each sequence
//! means, which is the part [`crate::vt`] deliberately doesn't do.
//!
//! # Replies
//!
//! Some sequences are questions. [`Emulator::consume`] collects the answers
//! into a buffer that the caller has to write back to the pty. This matters
//! more than it sounds: an Ink/React CLI (which is what `claude` is) asks for
//! the cursor position while starting up and waits for the answer.

use crate::helpers::*;
use crate::terminal::screen::{
    CellAttributes, Color, CursorStyle, EraseScope, MouseMode, Screen,
};
use crate::vt;

/// Sequences we saw but don't implement, recorded for debugging.
///
/// A terminal application that looks wrong is usually asking for something we
/// silently ignored, and guessing which one is miserable without this.
#[derive(Default)]
pub struct UnknownSequences {
    entries: Vec<String>,
}

impl UnknownSequences {
    /// At most this many distinct sequences are remembered.
    const LIMIT: usize = 32;

    fn record(&mut self, what: String) {
        if self.entries.len() < Self::LIMIT && !self.entries.contains(&what) {
            self.entries.push(what);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|s| s.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

pub struct Emulator {
    parser: vt::Parser,
    /// OSC and DCS payloads can be split across reads.
    string_buf: String,
    pub unknown: UnknownSequences,
}

impl Emulator {
    pub fn new() -> Self {
        Self { parser: vt::Parser::new(), string_buf: String::new(), unknown: Default::default() }
    }

    /// Applies `input` to `screen`, appending any replies to `reply`.
    pub fn consume(&mut self, screen: &mut Screen, input: &str, reply: &mut Vec<u8>) {
        let mut stream = self.parser.parse(input);

        while let Some(token) = stream.next() {
            match token {
                vt::Token::Text(text) => {
                    for ch in text.chars() {
                        screen.write_char(ch);
                    }
                }
                vt::Token::Ctrl(ch) => match ch {
                    '\r' => screen.carriage_return(),
                    '\n' | '\x0b' | '\x0c' => screen.line_feed(),
                    '\t' => screen.tab(),
                    '\x08' => screen.backspace(),
                    // Bell. Nothing to do; we're not going to beep at anyone.
                    '\x07' => {}
                    _ => {}
                },
                vt::Token::Esc(ch) => match ch {
                    '7' => screen.save_cursor(),
                    '8' => screen.restore_cursor(),
                    'M' => screen.reverse_index(),
                    'c' => screen.reset(),
                    // Character set selection. We're UTF-8 only, so the
                    // designators are noise, but they must not reach the screen.
                    '(' | ')' | '*' | '+' => {
                        stream.next_char();
                    }
                    '=' | '>' => {}
                    '\0' => {}
                    _ => self.unknown.record(format!("ESC {ch}")),
                },
                vt::Token::Csi(csi) => {
                    // `csi` borrows the parser, so the handler can't take
                    // `&mut self`. Everything it needs is passed explicitly.
                    Self::handle_csi(csi, screen, reply, &mut self.unknown);
                }
                vt::Token::Osc { data, partial } => {
                    if partial {
                        self.string_buf.push_str(data);
                        continue;
                    }
                    let complete = if self.string_buf.is_empty() {
                        data
                    } else {
                        self.string_buf.push_str(data);
                        &self.string_buf
                    };
                    handle_osc(complete, screen);
                    self.string_buf.clear();
                }
                // Device control strings are all things we don't speak.
                vt::Token::Dcs { partial, .. } => {
                    if !partial {
                        self.string_buf.clear();
                    }
                }
                vt::Token::SS3(_) => {}
            }
        }
    }

    fn handle_csi(
        csi: &vt::Csi,
        screen: &mut Screen,
        reply: &mut Vec<u8>,
        unknown: &mut UnknownSequences,
    ) {
        let params = &csi.params[..csi.param_count];

        // A missing or zero parameter means "the default", which for anything
        // that counts rows or columns is 1.
        let count = |i: usize| -> CoordType {
            match params.get(i) {
                Some(&0) | None => 1,
                Some(&v) => v as CoordType,
            }
        };
        // Positions are 1-based on the wire and 0-based here.
        let pos = |i: usize| -> CoordType { count(i) - 1 };

        if csi.private_byte == '?' {
            match csi.final_byte {
                'h' => set_dec_modes(params, screen, true),
                'l' => set_dec_modes(params, screen, false),
                // DECRQM. The `$` intermediate byte is dropped by the tokenizer,
                // but nothing else sends `CSI ? ... p`, so this is unambiguous.
                'p' => {
                    if let Some(&mode) = params.first() {
                        let state = report_dec_mode(mode, screen);
                        reply.extend_from_slice(format!("\x1b[?{mode};{state}$y").as_bytes());
                    }
                }
                // Selective erase. Treating it like a normal erase is wrong
                // only for applications that mark cells as protected, which
                // essentially nothing does anymore.
                'J' => screen.erase_in_display(erase_scope(params.first().copied())),
                'K' => screen.erase_in_line(erase_scope(params.first().copied())),
                _ => unknown.record(format!("CSI ? {:?} {}", params, csi.final_byte)),
            }
            return;
        }

        if csi.private_byte == '>' {
            match csi.final_byte {
                // XTVERSION. Ink probes this while working out what the terminal
                // can do; leaving it unanswered makes it fall back needlessly.
                'q' => reply.extend_from_slice(b"\x1bP>|edit\x1b\\"),
                // Secondary device attributes: a VT220 at firmware level 1.
                'c' => reply.extend_from_slice(b"\x1b[>0;10;1c"),
                _ => unknown.record(format!("CSI > {params:?} {}", csi.final_byte)),
            }
            return;
        }

        if csi.private_byte != '\0' {
            unknown.record(format!("CSI {} {:?} {}", csi.private_byte, params, csi.final_byte));
            return;
        }

        match csi.final_byte {
            // Cursor movement.
            'A' => screen.move_by(0, -count(0)),
            'B' | 'e' => screen.move_by(0, count(0)),
            'C' | 'a' => screen.move_by(count(0), 0),
            'D' => screen.move_by(-count(0), 0),
            'E' => {
                let dy = count(0);
                let y = screen.cursor().y + dy;
                screen.move_to(0, y);
            }
            'F' => {
                let dy = count(0);
                let y = screen.cursor().y - dy;
                screen.move_to(0, y);
            }
            'G' | '`' => {
                let y = screen.cursor().y;
                screen.move_to(pos(0), y);
            }
            'H' | 'f' => screen.move_to(pos(1), pos(0)),
            'd' => {
                let x = screen.cursor().x;
                screen.move_to(x, pos(0));
            }

            // Erasing and editing.
            'J' => screen.erase_in_display(erase_scope(params.first().copied())),
            'K' => screen.erase_in_line(erase_scope(params.first().copied())),
            'L' => screen.insert_lines(count(0)),
            'M' => screen.delete_lines(count(0)),
            'P' => screen.delete_chars(count(0)),
            'X' => screen.erase_chars(count(0)),
            '@' => screen.insert_chars(count(0)),

            // Scrolling.
            'S' => screen.scroll_up(count(0)),
            'T' => screen.scroll_down(count(0)),
            'r' => {
                let height = screen.size().height;
                let top = pos(0);
                let bottom = match params.get(1) {
                    Some(&0) | None => height,
                    Some(&v) => v as CoordType,
                };
                screen.set_scroll_region(top, bottom);
                // DECSTBM homes the cursor.
                screen.move_to(0, top);
            }

            // Rendition.
            'm' => apply_sgr(params, screen),

            // Reports.
            'c' => {
                // Device attributes: "I am a VT100 with an advanced video option."
                reply.extend_from_slice(b"\x1b[?1;2c");
            }
            'n' => {
                match params.first() {
                    // Device status: we're fine.
                    Some(&5) => reply.extend_from_slice(b"\x1b[0n"),
                    // Cursor position. Ink asks for this on startup and waits.
                    Some(&6) => {
                        let c = screen.cursor();
                        reply.extend_from_slice(
                            format!("\x1b[{};{}R", c.y + 1, c.x + 1).as_bytes(),
                        );
                    }
                    _ => {}
                }
            }

            // DECSCUSR, whose ` ` intermediate byte the tokenizer drops.
            'q' => {
                screen.cursor_style = match params.first().copied().unwrap_or(0) {
                    3 | 4 => CursorStyle::Underline,
                    5 | 6 => CursorStyle::Bar,
                    _ => CursorStyle::Block,
                };
            }

            // ANSI modes. Only insert mode would be interesting and nothing uses it.
            'h' | 'l' => {}
            // Tab stop handling; we hardcode stops every 8 columns.
            'g' => {}

            _ => unknown.record(format!("CSI {:?} {}", params, csi.final_byte)),
        }
    }
}

impl Default for Emulator {
    fn default() -> Self {
        Self::new()
    }
}

fn erase_scope(param: Option<u16>) -> EraseScope {
    match param {
        Some(1) => EraseScope::ToStart,
        Some(2) | Some(3) => EraseScope::All,
        _ => EraseScope::ToEnd,
    }
}

fn set_dec_modes(params: &[u16], screen: &mut Screen, on: bool) {
    for &param in params {
        match param {
            // DECCKM: arrow keys send SS3 instead of CSI.
            1 => screen.application_cursor_keys = on,
            // DECAWM.
            7 => screen.set_autowrap(on),
            // DECTCEM.
            25 => screen.cursor_visible = on,
            1000 => screen.mouse_mode = if on { MouseMode::Buttons } else { MouseMode::Off },
            1002 => screen.mouse_mode = if on { MouseMode::Drag } else { MouseMode::Off },
            1003 => screen.mouse_mode = if on { MouseMode::Motion } else { MouseMode::Off },
            // SGR mouse encoding. We always report in SGR form, so there's
            // nothing to switch, but applications do enable it explicitly.
            1006 => {}
            // Alternate screen. 47 and 1047 are the older, cursor-less variants.
            47 | 1047 | 1049 => screen.set_alternate(on),
            1048 => {
                if on {
                    screen.save_cursor()
                } else {
                    screen.restore_cursor()
                }
            }
            2004 => screen.bracketed_paste = on,
            // Synchronized output. Nothing to do: a frame is only ever shown
            // once we've consumed everything the child sent us.
            2026 => screen.synchronized_output = on,
            _ => {}
        }
    }
}

/// Answers DECRQM for a DEC private mode.
///
/// 0 means "I've never heard of it", 1 set, 2 reset. Saying 0 for a mode we do
/// support would make applications fall back to worse behaviour.
fn report_dec_mode(mode: u16, screen: &Screen) -> u16 {
    let set = match mode {
        1 => screen.application_cursor_keys,
        7 => screen.autowrap(),
        25 => screen.cursor_visible,
        1000 => screen.mouse_mode == MouseMode::Buttons,
        1002 => screen.mouse_mode == MouseMode::Drag,
        1003 => screen.mouse_mode == MouseMode::Motion,
        47 | 1047 | 1049 => screen.on_alternate(),
        2004 => screen.bracketed_paste,
        2026 => screen.synchronized_output,
        _ => return 0,
    };
    if set { 1 } else { 2 }
}

/// Applies an SGR (Select Graphic Rendition) sequence.
fn apply_sgr(params: &[u16], screen: &mut Screen) {
    if params.is_empty() {
        screen.reset_pen();
        return;
    }

    let mut i = 0;
    while i < params.len() {
        let param = params[i];
        let pen = screen.pen_mut();

        match param {
            0 => *pen = Default::default(),
            1 => pen.attr = pen.attr.with(CellAttributes::BOLD),
            2 => pen.attr = pen.attr.with(CellAttributes::DIM),
            3 => pen.attr = pen.attr.with(CellAttributes::ITALIC),
            4 => pen.attr = pen.attr.with(CellAttributes::UNDERLINE),
            7 => pen.attr = pen.attr.with(CellAttributes::REVERSE),
            9 => pen.attr = pen.attr.with(CellAttributes::STRIKETHROUGH),
            22 => pen.attr = pen.attr.without(CellAttributes::BOLD.with(CellAttributes::DIM)),
            23 => pen.attr = pen.attr.without(CellAttributes::ITALIC),
            24 => pen.attr = pen.attr.without(CellAttributes::UNDERLINE),
            27 => pen.attr = pen.attr.without(CellAttributes::REVERSE),
            29 => pen.attr = pen.attr.without(CellAttributes::STRIKETHROUGH),

            30..=37 => pen.fg = Color::indexed(param as u8 - 30),
            39 => pen.fg = Color::DEFAULT,
            40..=47 => pen.bg = Color::indexed(param as u8 - 40),
            49 => pen.bg = Color::DEFAULT,
            90..=97 => pen.fg = Color::indexed(param as u8 - 90 + 8),
            100..=107 => pen.bg = Color::indexed(param as u8 - 100 + 8),

            // Extended colors: `38;5;n` or `38;2;r;g;b`.
            38 | 48 => {
                let foreground = param == 38;
                let Some(color) = parse_extended_color(params, &mut i) else {
                    return;
                };
                let pen = screen.pen_mut();
                if foreground {
                    pen.fg = color;
                } else {
                    pen.bg = color;
                }
            }
            _ => {}
        }

        i += 1;
    }
}

/// Reads the tail of a `38`/`48` SGR parameter, advancing `i` past it.
///
/// Returns `None` if the sequence is truncated, in which case the rest of the
/// SGR sequence is meaningless anyway.
fn parse_extended_color(params: &[u16], i: &mut usize) -> Option<Color> {
    match params.get(*i + 1) {
        Some(&5) => {
            let index = *params.get(*i + 2)? as u8;
            *i += 2;
            Some(Color::indexed(index))
        }
        Some(&2) => {
            let r = *params.get(*i + 2)? as u32;
            let g = *params.get(*i + 3)? as u32;
            let b = *params.get(*i + 4)? as u32;
            *i += 4;
            Some(Color::rgb((r << 16) | (g << 8) | b))
        }
        _ => None,
    }
}

fn handle_osc(data: &str, screen: &mut Screen) {
    let (command, payload) = data.split_once(';').unwrap_or((data, ""));
    match command {
        // Window title. 0 also sets the icon name, which we have no use for.
        "0" | "2" => {
            screen.title.clear();
            screen.title.push_str(payload);
        }
        // Hyperlinks, clipboard, color queries: recognised and ignored.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::screen::DEFAULT_SCROLLBACK;

    const SIZE: Size = Size { width: 20, height: 5 };

    struct Harness {
        emulator: Emulator,
        screen: Screen,
        reply: Vec<u8>,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                emulator: Emulator::new(),
                screen: Screen::new(SIZE, DEFAULT_SCROLLBACK),
                reply: Vec::new(),
            }
        }

        fn feed(&mut self, input: &str) -> &mut Self {
            self.emulator.consume(&mut self.screen, input, &mut self.reply);
            self
        }

        fn line(&self, y: CoordType) -> String {
            self.screen
                .visible_row(y)
                .iter()
                .filter(|c| !c.attr.has(CellAttributes::WIDE_TRAILER))
                .map(|c| c.ch)
                .collect::<String>()
                .trim_end()
                .to_string()
        }

        fn reply(&self) -> String {
            String::from_utf8_lossy(&self.reply).into_owned()
        }
    }

    #[test]
    fn plain_text_and_newlines() {
        let mut h = Harness::new();
        h.feed("hello\r\nworld");
        assert_eq!(h.line(0), "hello");
        assert_eq!(h.line(1), "world");
    }

    #[test]
    fn carriage_return_overwrites_in_place() {
        // This is how every progress bar works.
        let mut h = Harness::new();
        h.feed("50%\r100%");
        assert_eq!(h.line(0), "100%");
    }

    #[test]
    fn cursor_positioning_is_one_based() {
        let mut h = Harness::new();
        h.feed("\x1b[3;5Hx");
        assert_eq!(h.screen.cursor(), Point { x: 5, y: 2 });
        assert_eq!(h.line(2), "    x");

        // Missing parameters mean "home".
        h.feed("\x1b[H");
        assert_eq!(h.screen.cursor(), Point { x: 0, y: 0 });
    }

    #[test]
    fn cursor_movement_defaults_to_one() {
        let mut h = Harness::new();
        h.feed("\x1b[5;5H");
        h.feed("\x1b[A");
        assert_eq!(h.screen.cursor().y, 3);
        h.feed("\x1b[2B");
        assert_eq!(h.screen.cursor().y, 5 - 1);
        h.feed("\x1b[3D");
        assert_eq!(h.screen.cursor().x, 1);
    }

    #[test]
    fn erase_in_display_scopes() {
        let mut h = Harness::new();
        h.feed("aaa\r\nbbb\r\nccc");
        h.feed("\x1b[2;2H\x1b[J");
        assert_eq!(h.line(0), "aaa");
        assert_eq!(h.line(1), "b");
        assert_eq!(h.line(2), "");

        h.feed("\x1b[2J");
        assert_eq!(h.line(0), "");
    }

    #[test]
    fn sgr_sets_and_resets_colors() {
        let mut h = Harness::new();
        h.feed("\x1b[31;1mred\x1b[0mplain");

        let row = h.screen.visible_row(0);
        assert_eq!(row[0].fg, Color::indexed(1));
        assert!(row[0].attr.has(CellAttributes::BOLD));
        assert_eq!(row[3].fg, Color::DEFAULT);
        assert!(!row[3].attr.has(CellAttributes::BOLD));
    }

    #[test]
    fn sgr_bright_and_background_colors() {
        let mut h = Harness::new();
        h.feed("\x1b[92;44mx");
        let cell = h.screen.visible_row(0)[0];
        assert_eq!(cell.fg, Color::indexed(10));
        assert_eq!(cell.bg, Color::indexed(4));
    }

    #[test]
    fn sgr_256_and_truecolor() {
        let mut h = Harness::new();
        h.feed("\x1b[38;5;208ma");
        assert_eq!(h.screen.visible_row(0)[0].fg, Color::indexed(208));

        h.feed("\x1b[48;2;18;52;86mb");
        assert_eq!(h.screen.visible_row(0)[1].bg, Color::rgb(0x123456));

        // A truncated extended color must not corrupt the rest of the pen.
        h.feed("\x1b[38;5m");
        assert_eq!(h.screen.visible_row(0)[1].bg, Color::rgb(0x123456));
    }

    #[test]
    fn sgr_with_no_params_resets() {
        let mut h = Harness::new();
        h.feed("\x1b[31m\x1b[max");
        assert_eq!(h.screen.visible_row(0)[0].fg, Color::DEFAULT);
    }

    #[test]
    fn alternate_screen_round_trip() {
        let mut h = Harness::new();
        h.feed("primary");
        h.feed("\x1b[?1049h");
        assert_eq!(h.line(0), "");
        h.feed("alt");
        assert_eq!(h.line(0), "alt");
        h.feed("\x1b[?1049l");
        assert_eq!(h.line(0), "primary");
    }

    #[test]
    fn dec_modes_toggle_state() {
        let mut h = Harness::new();
        h.feed("\x1b[?25l");
        assert!(!h.screen.cursor_visible);
        h.feed("\x1b[?25h");
        assert!(h.screen.cursor_visible);

        h.feed("\x1b[?2004h");
        assert!(h.screen.bracketed_paste);

        h.feed("\x1b[?1002h");
        assert_eq!(h.screen.mouse_mode, MouseMode::Drag);
        h.feed("\x1b[?1002l");
        assert_eq!(h.screen.mouse_mode, MouseMode::Off);

        h.feed("\x1b[?1h");
        assert!(h.screen.application_cursor_keys);
    }

    #[test]
    fn cursor_position_report_answers() {
        let mut h = Harness::new();
        h.feed("\x1b[3;7H\x1b[6n");
        assert_eq!(h.reply(), "\x1b[3;7R");
    }

    #[test]
    fn device_attributes_answer() {
        let mut h = Harness::new();
        h.feed("\x1b[c");
        assert_eq!(h.reply(), "\x1b[?1;2c");
    }

    #[test]
    fn cursor_style_is_tracked() {
        let mut h = Harness::new();
        h.feed("\x1b[5 q");
        assert_eq!(h.screen.cursor_style, CursorStyle::Bar);
        h.feed("\x1b[4 q");
        assert_eq!(h.screen.cursor_style, CursorStyle::Underline);
        h.feed("\x1b[0 q");
        assert_eq!(h.screen.cursor_style, CursorStyle::Block);
    }

    #[test]
    fn mode_queries_are_answered() {
        // Claude Code asks whether synchronized output (2026) works.
        let mut h = Harness::new();
        h.feed("\x1b[?2026$p");
        assert_eq!(h.reply(), "\x1b[?2026;2$y");

        let mut h = Harness::new();
        h.feed("\x1b[?2026h\x1b[?2026$p");
        assert_eq!(h.reply(), "\x1b[?2026;1$y");

        // A mode we genuinely don't have must report "unrecognised".
        let mut h = Harness::new();
        h.feed("\x1b[?9999$p");
        assert_eq!(h.reply(), "\x1b[?9999;0$y");
    }

    #[test]
    fn terminal_identification_answers() {
        // Claude Code sends `CSI > q` on startup.
        let mut h = Harness::new();
        h.feed("\x1b[>q");
        assert_eq!(h.reply(), "\x1bP>|edit\x1b\\");

        let mut h = Harness::new();
        h.feed("\x1b[>c");
        assert_eq!(h.reply(), "\x1b[>0;10;1c");
    }

    #[test]
    fn scroll_region_and_reverse_index() {
        let mut h = Harness::new();
        h.feed("l0\r\nl1\r\nl2\r\nl3\r\nl4");
        // Region rows 2..4 (1-based), cursor homes to its top.
        h.feed("\x1b[2;4r");
        assert_eq!(h.screen.cursor(), Point { x: 0, y: 1 });
        h.feed("\x1bM");
        assert_eq!(h.line(0), "l0");
        assert_eq!(h.line(1), "");
        assert_eq!(h.line(2), "l1");
        assert_eq!(h.line(4), "l4");
    }

    #[test]
    fn insert_and_delete_lines() {
        let mut h = Harness::new();
        h.feed("a\r\nb\r\nc");
        h.feed("\x1b[2;1H\x1b[L");
        assert_eq!(h.line(1), "");
        assert_eq!(h.line(2), "b");

        h.feed("\x1b[M");
        assert_eq!(h.line(1), "b");
    }

    #[test]
    fn osc_sets_the_title() {
        let mut h = Harness::new();
        h.feed("\x1b]0;my title\x07");
        assert_eq!(h.screen.title, "my title");

        h.feed("\x1b]2;other\x1b\\");
        assert_eq!(h.screen.title, "other");
    }

    #[test]
    fn osc_split_across_reads() {
        let mut h = Harness::new();
        h.feed("\x1b]0;split ");
        h.feed("title\x07");
        assert_eq!(h.screen.title, "split title");
    }

    #[test]
    fn sequences_split_across_reads() {
        // The pty hands us arbitrary chunks, so every sequence can be cut apart.
        let mut h = Harness::new();
        h.feed("\x1b[");
        h.feed("31m");
        h.feed("x");
        assert_eq!(h.screen.visible_row(0)[0].fg, Color::indexed(1));
    }

    #[test]
    fn charset_designators_do_not_leak_into_the_screen() {
        let mut h = Harness::new();
        h.feed("\x1b(Bhello");
        assert_eq!(h.line(0), "hello");
    }

    #[test]
    fn unknown_sequences_are_recorded_once() {
        let mut h = Harness::new();
        h.feed("\x1b[99Z");
        h.feed("\x1b[99Z");
        let seen: Vec<_> = h.emulator.unknown.iter().collect();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].contains('Z'), "{seen:?}");
    }

    #[test]
    fn full_reset_clears_everything() {
        let mut h = Harness::new();
        h.feed("\x1b[31mstuff\x1b[?25l");
        h.feed("\x1bc");
        assert_eq!(h.line(0), "");
        assert!(h.screen.cursor_visible);
        assert_eq!(h.screen.cursor(), Point { x: 0, y: 0 });
    }
}
