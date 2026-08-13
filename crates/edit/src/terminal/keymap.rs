// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Turns editor input events back into the bytes a terminal would send.
//!
//! The editor's input parser decoded VT into [`InputKey`]s for its own use;
//! here we encode them again for the child process. Going through the parsed
//! form rather than forwarding the raw bytes means the panel can intercept
//! individual keys (the one that moves focus back out) and that mouse and
//! paste events get re-encoded in whatever form the child asked for.

use crate::helpers::*;
use crate::input::{InputKey, InputKeyMod, InputMouseState, kbmod, vk};
use crate::terminal::screen::{MouseMode, Screen};

/// Encodes a key press, or returns `None` if there's nothing sensible to send.
pub fn encode_key(key: InputKey, screen: &Screen) -> Option<Vec<u8>> {
    let modifiers = key.modifiers();
    let base = key.key();
    let ctrl = modifiers.contains(kbmod::CTRL);
    let alt = modifiers.contains(kbmod::ALT);
    let shift = modifiers.contains(kbmod::SHIFT);

    // xterm's modifier parameter: 1 + bitmask, sent as the second CSI argument.
    let modifier_param = 1 + (shift as u8) + 2 * (alt as u8) + 4 * (ctrl as u8);

    // Cursor and edit keys. `application_cursor_keys` swaps the CSI
    // introducer for SS3, which is what full screen applications expect.
    let cursor = |final_byte: u8| -> Vec<u8> {
        if modifier_param > 1 {
            format!("\x1b[1;{modifier_param}{}", final_byte as char).into_bytes()
        } else if screen.application_cursor_keys {
            vec![0x1b, b'O', final_byte]
        } else {
            vec![0x1b, b'[', final_byte]
        }
    };

    // The `CSI n ~` family.
    let tilde = |number: u8| -> Vec<u8> {
        if modifier_param > 1 {
            format!("\x1b[{number};{modifier_param}~").into_bytes()
        } else {
            format!("\x1b[{number}~").into_bytes()
        }
    };

    let bytes = match base {
        vk::UP => cursor(b'A'),
        vk::DOWN => cursor(b'B'),
        vk::RIGHT => cursor(b'C'),
        vk::LEFT => cursor(b'D'),
        vk::END => cursor(b'F'),
        vk::HOME => cursor(b'H'),

        vk::INSERT => tilde(2),
        vk::DELETE => tilde(3),
        vk::PRIOR => tilde(5),
        vk::NEXT => tilde(6),

        vk::F1 => function_key(b'P', 11, modifier_param),
        vk::F2 => function_key(b'Q', 12, modifier_param),
        vk::F3 => function_key(b'R', 13, modifier_param),
        vk::F4 => function_key(b'S', 14, modifier_param),
        vk::F5 => tilde(15),
        vk::F6 => tilde(17),
        vk::F7 => tilde(18),
        vk::F8 => tilde(19),
        vk::F9 => tilde(20),
        vk::F10 => tilde(21),
        vk::F11 => tilde(23),
        vk::F12 => tilde(24),

        vk::RETURN => vec![b'\r'],
        vk::TAB => {
            if shift {
                // Back-tab.
                b"\x1b[Z".to_vec()
            } else {
                vec![b'\t']
            }
        }
        // Terminals send DEL for backspace, not BS. Sending the wrong one
        // makes readline-style editors delete forwards or nothing at all.
        vk::BACK => vec![if ctrl { 0x08 } else { 0x7f }],
        vk::ESCAPE => vec![0x1b],

        vk::SPACE if ctrl => vec![0],

        // Ctrl+A..Z and the handful of punctuation controls around them.
        key if ctrl && key.value() >= 'A' as u32 && key.value() <= 'Z' as u32 => {
            vec![(key.value() - 'A' as u32 + 1) as u8]
        }

        // Anything else printable is delivered as `Input::Text` instead,
        // which keeps dead keys and IME composition working.
        _ => return None,
    };

    Some(if alt && !bytes.starts_with(&[0x1b]) {
        // Alt is "meta sends escape": prefix rather than a modifier parameter.
        let mut with_esc = Vec::with_capacity(bytes.len() + 1);
        with_esc.push(0x1b);
        with_esc.extend_from_slice(&bytes);
        with_esc
    } else {
        bytes
    })
}

/// F1-F4 are SS3 sequences until a modifier gets involved.
fn function_key(ss3: u8, number: u8, modifier_param: u8) -> Vec<u8> {
    if modifier_param > 1 {
        format!("\x1b[{number};{modifier_param}~").into_bytes()
    } else {
        vec![0x1b, b'O', ss3]
    }
}

/// Encodes literal text, honouring the child's bracketed paste mode.
pub fn encode_paste(text: &[u8], screen: &Screen) -> Vec<u8> {
    if !screen.bracketed_paste {
        return text.to_vec();
    }

    let mut out = Vec::with_capacity(text.len() + 12);
    out.extend_from_slice(b"\x1b[200~");
    out.extend_from_slice(text);
    out.extend_from_slice(b"\x1b[201~");
    out
}

/// A mouse event, already translated into the terminal's coordinate system.
pub struct MouseEvent {
    pub state: InputMouseState,
    pub modifiers: InputKeyMod,
    /// Relative to the terminal's top left cell.
    pub position: Point,
    /// Wheel movement, negative meaning "away from the user".
    pub scroll: Point,
    /// Whether a button is being held while moving.
    pub drag: bool,
}

/// Encodes a mouse event in SGR form, or `None` if the child didn't ask for
/// mouse reporting or wouldn't care about this particular event.
pub fn encode_mouse(mouse: &MouseEvent, screen: &Screen) -> Option<Vec<u8>> {
    if screen.mouse_mode == MouseMode::Off {
        return None;
    }

    // Wheel events are buttons 64 and 65 by convention.
    if mouse.state == InputMouseState::Scroll || mouse.scroll.y != 0 {
        let button = if mouse.scroll.y < 0 { 64 } else { 65 };
        let count = mouse.scroll.y.abs().max(1);
        let mut out = Vec::new();
        for _ in 0..count {
            out.extend_from_slice(
                sgr_mouse(button, mouse.modifiers, mouse.position, true).as_bytes(),
            );
        }
        return Some(out);
    }

    let (button, press) = match mouse.state {
        InputMouseState::Left => (0, true),
        InputMouseState::Middle => (1, true),
        InputMouseState::Right => (2, true),
        // A release doesn't say which button it was, and 0 is the safe guess.
        InputMouseState::Release => (0, false),
        InputMouseState::None | InputMouseState::Scroll => return None,
    };

    // Motion is only reported when the application asked for it.
    let button = if mouse.drag {
        if screen.mouse_mode == MouseMode::Buttons {
            return None;
        }
        button + 32
    } else {
        button
    };

    Some(sgr_mouse(button, mouse.modifiers, mouse.position, press).into_bytes())
}

fn sgr_mouse(button: u32, modifiers: InputKeyMod, position: Point, press: bool) -> String {
    let mut button = button;
    if modifiers.contains(kbmod::SHIFT) {
        button += 4;
    }
    if modifiers.contains(kbmod::ALT) {
        button += 8;
    }
    if modifiers.contains(kbmod::CTRL) {
        button += 16;
    }

    format!(
        "\x1b[<{button};{};{}{}",
        position.x + 1,
        position.y + 1,
        if press { 'M' } else { 'm' }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::screen::DEFAULT_SCROLLBACK;

    fn screen() -> Screen {
        Screen::new(Size { width: 80, height: 25 }, DEFAULT_SCROLLBACK)
    }

    fn encode(key: InputKey) -> Option<String> {
        encode_key(key, &screen()).map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    #[test]
    fn plain_control_keys() {
        assert_eq!(encode(vk::RETURN).as_deref(), Some("\r"));
        assert_eq!(encode(vk::TAB).as_deref(), Some("\t"));
        assert_eq!(encode(vk::ESCAPE).as_deref(), Some("\x1b"));
        // Backspace must be DEL, or shells delete the wrong way.
        assert_eq!(encode(vk::BACK).as_deref(), Some("\x7f"));
    }

    #[test]
    fn arrows_switch_on_application_mode() {
        let mut s = screen();
        assert_eq!(
            encode_key(vk::UP, &s).map(|b| String::from_utf8_lossy(&b).into_owned()).as_deref(),
            Some("\x1b[A")
        );

        s.application_cursor_keys = true;
        assert_eq!(
            encode_key(vk::UP, &s).map(|b| String::from_utf8_lossy(&b).into_owned()).as_deref(),
            Some("\x1bOA")
        );
    }

    #[test]
    fn modifiers_use_the_xterm_parameter_form() {
        assert_eq!(encode(kbmod::CTRL | vk::RIGHT).as_deref(), Some("\x1b[1;5C"));
        assert_eq!(encode(kbmod::SHIFT | vk::UP).as_deref(), Some("\x1b[1;2A"));
        assert_eq!(encode(kbmod::CTRL_SHIFT | vk::LEFT).as_deref(), Some("\x1b[1;6D"));
    }

    #[test]
    fn ctrl_letters_become_control_codes() {
        assert_eq!(encode(kbmod::CTRL | vk::C).as_deref(), Some("\x03"));
        assert_eq!(encode(kbmod::CTRL | vk::A).as_deref(), Some("\x01"));
        assert_eq!(encode(kbmod::CTRL | vk::Z).as_deref(), Some("\x1a"));
        assert_eq!(encode(kbmod::CTRL | vk::SPACE).as_deref(), Some("\0"));
    }

    #[test]
    fn alt_prefixes_with_escape() {
        assert_eq!(encode(kbmod::ALT | vk::RETURN).as_deref(), Some("\x1b\r"));
        // ...but a sequence that already starts with ESC uses the parameter form.
        assert_eq!(encode(kbmod::ALT | vk::UP).as_deref(), Some("\x1b[1;3A"));
    }

    #[test]
    fn navigation_and_function_keys() {
        assert_eq!(encode(vk::HOME).as_deref(), Some("\x1b[H"));
        assert_eq!(encode(vk::DELETE).as_deref(), Some("\x1b[3~"));
        assert_eq!(encode(vk::PRIOR).as_deref(), Some("\x1b[5~"));
        assert_eq!(encode(vk::F1).as_deref(), Some("\x1bOP"));
        assert_eq!(encode(vk::F5).as_deref(), Some("\x1b[15~"));
        assert_eq!(encode(vk::F12).as_deref(), Some("\x1b[24~"));
        assert_eq!(encode(kbmod::SHIFT | vk::TAB).as_deref(), Some("\x1b[Z"));
    }

    #[test]
    fn printable_keys_are_left_to_the_text_path() {
        // Otherwise every letter would be sent twice.
        assert_eq!(encode(vk::A), None);
        assert_eq!(encode(vk::N1), None);
    }

    #[test]
    fn paste_is_bracketed_only_when_asked() {
        let mut s = screen();
        assert_eq!(encode_paste(b"hi", &s), b"hi".to_vec());

        s.bracketed_paste = true;
        assert_eq!(encode_paste(b"hi", &s), b"\x1b[200~hi\x1b[201~".to_vec());
    }

    fn mouse_at(state: InputMouseState) -> MouseEvent {
        MouseEvent {
            state,
            modifiers: kbmod::NONE,
            position: Point { x: 3, y: 4 },
            scroll: Point { x: 0, y: 0 },
            drag: false,
        }
    }

    #[test]
    fn mouse_is_silent_unless_requested() {
        let s = screen();
        assert!(encode_mouse(&mouse_at(InputMouseState::Left), &s).is_none());
    }

    #[test]
    fn mouse_uses_sgr_encoding() {
        let mut s = screen();
        s.mouse_mode = MouseMode::Drag;

        let encoded = encode_mouse(&mouse_at(InputMouseState::Left), &s).unwrap();
        assert_eq!(String::from_utf8_lossy(&encoded), "\x1b[<0;4;5M");

        let encoded = encode_mouse(&mouse_at(InputMouseState::Release), &s).unwrap();
        assert_eq!(String::from_utf8_lossy(&encoded), "\x1b[<0;4;5m");

        // Dragging only reports in the modes that asked for motion.
        let drag = MouseEvent { drag: true, ..mouse_at(InputMouseState::Left) };
        let encoded = encode_mouse(&drag, &s).unwrap();
        assert_eq!(String::from_utf8_lossy(&encoded), "\x1b[<32;4;5M");

        s.mouse_mode = MouseMode::Buttons;
        assert!(encode_mouse(&drag, &s).is_none());
    }

    #[test]
    fn scroll_wheel_repeats_per_notch() {
        let mut s = screen();
        s.mouse_mode = MouseMode::Buttons;

        let up = MouseEvent {
            position: Point { x: 0, y: 0 },
            scroll: Point { x: 0, y: -2 },
            ..mouse_at(InputMouseState::Scroll)
        };
        let encoded = encode_mouse(&up, &s).unwrap();
        assert_eq!(String::from_utf8_lossy(&encoded), "\x1b[<64;1;1M\x1b[<64;1;1M");
    }
}
