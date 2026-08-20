// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Turning a key written in `settings.json` into one the editor can match.
//!
//! A default shortcut is only a guess about someone else's keyboard, and the
//! guesses go wrong in ways nobody can predict: `Ctrl+Space` is the
//! conventional key for completion and it is also what Microsoft's IME uses to
//! switch between Chinese and English, so on a machine with that installed the
//! editor never sees it. No default is right everywhere; being able to say
//! otherwise is.
//!
//! ```jsonc
//! "completion.next": "alt+n",
//! "completion.prev": "alt+p"
//! ```
//!
//! # What can be bound
//!
//! Letters, digits, function keys, and the named keys below, with any of
//! `ctrl`, `alt` and `shift`.
//!
//! **Punctuation cannot**, and the reason is worth knowing before reaching for
//! it: a terminal has no way to send most `Ctrl`+punctuation combinations in
//! the first place -- only `Ctrl`+letter has a control code -- and this
//! editor's [`InputKey`] has no representation for them either. Binding one
//! would produce a shortcut that silently never fires, so [`parse`] refuses it
//! instead.

use edit::input::{InputKey, InputKeyMod, kbmod, vk};

/// Every key that can be named, and the key it means.
///
/// A table rather than arithmetic on key codes, because [`InputKey`]'s
/// constructor is private to the library -- which is also what stops anything
/// here from inventing a key the input parser will never produce.
const KEYS: &[(&str, InputKey)] = &[
    ("space", vk::SPACE),
    ("tab", vk::TAB),
    ("enter", vk::RETURN),
    ("return", vk::RETURN),
    ("escape", vk::ESCAPE),
    ("esc", vk::ESCAPE),
    ("backspace", vk::BACK),
    ("insert", vk::INSERT),
    ("delete", vk::DELETE),
    ("home", vk::HOME),
    ("end", vk::END),
    ("pageup", vk::PRIOR),
    ("pagedown", vk::NEXT),
    ("up", vk::UP),
    ("down", vk::DOWN),
    ("left", vk::LEFT),
    ("right", vk::RIGHT),
    ("f1", vk::F1),
    ("f2", vk::F2),
    ("f3", vk::F3),
    ("f4", vk::F4),
    ("f5", vk::F5),
    ("f6", vk::F6),
    ("f7", vk::F7),
    ("f8", vk::F8),
    ("f9", vk::F9),
    ("f10", vk::F10),
    ("f11", vk::F11),
    ("f12", vk::F12),
    ("a", vk::A),
    ("b", vk::B),
    ("c", vk::C),
    ("d", vk::D),
    ("e", vk::E),
    ("f", vk::F),
    ("g", vk::G),
    ("h", vk::H),
    ("i", vk::I),
    ("j", vk::J),
    ("k", vk::K),
    ("l", vk::L),
    ("m", vk::M),
    ("n", vk::N),
    ("o", vk::O),
    ("p", vk::P),
    ("q", vk::Q),
    ("r", vk::R),
    ("s", vk::S),
    ("t", vk::T),
    ("u", vk::U),
    ("v", vk::V),
    ("w", vk::W),
    ("x", vk::X),
    ("y", vk::Y),
    ("z", vk::Z),
    ("0", vk::N0),
    ("1", vk::N1),
    ("2", vk::N2),
    ("3", vk::N3),
    ("4", vk::N4),
    ("5", vk::N5),
    ("6", vk::N6),
    ("7", vk::N7),
    ("8", vk::N8),
    ("9", vk::N9),
];

/// The modifier combinations, for naming a key back to the user.
const MODIFIERS: &[(&str, InputKeyMod)] = &[
    ("", kbmod::NONE),
    ("Ctrl+", kbmod::CTRL),
    ("Alt+", kbmod::ALT),
    ("Shift+", kbmod::SHIFT),
    ("Ctrl+Alt+", kbmod::CTRL_ALT),
    ("Ctrl+Shift+", kbmod::CTRL_SHIFT),
    ("Alt+Shift+", kbmod::ALT_SHIFT),
    ("Ctrl+Alt+Shift+", kbmod::CTRL_ALT_SHIFT),
];

/// Parses a key description like `ctrl+e`, `alt+n`, `f4` or `ctrl+space`.
///
/// Case and spacing don't matter. Returns `None` for anything that can't be
/// bound, which the caller reports rather than ignores -- a shortcut that
/// quietly never fires is the worst outcome here.
pub fn parse(spec: &str) -> Option<InputKey> {
    let mut modifiers = kbmod::NONE;
    let mut key = None;

    for part in spec.split('+') {
        let part = part.trim().to_ascii_lowercase();
        if part.is_empty() {
            return None;
        }

        match part.as_str() {
            "ctrl" | "control" => modifiers |= kbmod::CTRL,
            "alt" | "meta" => modifiers |= kbmod::ALT,
            "shift" => modifiers |= kbmod::SHIFT,
            name => {
                // Only one non-modifier part is meaningful.
                if key.is_some() {
                    return None;
                }
                key = Some(KEYS.iter().find(|(n, _)| *n == name)?.1);
            }
        }
    }

    Some(modifiers | key?)
}

/// Names a key the way [`parse`] would accept it back.
///
/// Used by the key probe, which is how someone finds out what their terminal
/// and their IME actually leave available.
pub fn describe(key: InputKey) -> Option<String> {
    // Rebuilt by comparison rather than by taking the key apart, because the
    // accessors for that are private to the library. There are only a few
    // hundred combinations, and this runs once per keypress.
    for (mod_name, modifier) in MODIFIERS {
        for (name, base) in KEYS {
            if *modifier | *base == key {
                return Some(format!("{mod_name}{name}"));
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_plain_key() {
        assert_eq!(parse("e"), Some(vk::E));
        // Case doesn't matter: the input parser reports letters upper case.
        assert_eq!(parse("E"), parse("e"));
    }

    #[test]
    fn parses_modifiers() {
        assert_eq!(parse("ctrl+e"), Some(kbmod::CTRL | vk::E));
        assert_eq!(parse("alt+n"), Some(kbmod::ALT | vk::N));
        assert_eq!(parse("ctrl+shift+p"), Some(kbmod::CTRL_SHIFT | vk::P));
    }

    #[test]
    fn is_forgiving_about_spacing_and_case() {
        assert_eq!(parse("  CTRL + Space "), Some(kbmod::CTRL | vk::SPACE));
        assert_eq!(parse("Control+E"), parse("ctrl+e"));
    }

    #[test]
    fn parses_named_keys() {
        assert_eq!(parse("space"), Some(vk::SPACE));
        assert_eq!(parse("f4"), Some(vk::F4));
        assert_eq!(parse("ctrl+f4"), Some(kbmod::CTRL | vk::F4));
        assert_eq!(parse("pagedown"), Some(vk::NEXT));
    }

    #[test]
    fn rejects_what_it_cannot_bind() {
        assert_eq!(parse("ctrl+nope"), None);
        assert_eq!(parse("ctrl+"), None);
        assert_eq!(parse("ctrl"), None, "a modifier alone is not a key");
        assert_eq!(parse("a+b"), None, "two keys is not a shortcut");
        assert_eq!(parse(""), None);
        assert_eq!(parse("f99"), None, "must not silently become some other key");
    }

    #[test]
    fn punctuation_is_refused_rather_than_mangled() {
        // A terminal can't send these and the editor can't represent them, so
        // binding one would mean a shortcut that never fires. Saying no is
        // kinder than accepting it.
        assert_eq!(parse("ctrl+."), None);
        assert_eq!(parse("ctrl+,"), None);
        assert_eq!(parse("alt+/"), None);
    }

    #[test]
    fn describes_what_it_parses() {
        for spec in ["ctrl+e", "alt+n", "f4", "ctrl+space", "ctrl+shift+p", "up"] {
            let key = parse(spec).unwrap();
            let described = describe(key).expect("every bindable key has a name");
            assert_eq!(parse(&described), Some(key), "{spec} described as {described}");
        }
    }

    #[test]
    fn every_name_in_the_table_parses() {
        for (name, key) in KEYS {
            assert_eq!(parse(name), Some(*key), "{name}");
        }
    }
}
