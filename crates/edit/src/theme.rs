// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Syntax highlighting colors.
//!
//! A theme maps each [`HighlightKind`] onto one of the terminal's 16 palette
//! colors. Deliberately *not* onto arbitrary RGB: the palette entries are
//! resolved against whatever the host terminal reports (see the OSC 4/10/11
//! queries in the editor's startup), so the editor keeps looking like it
//! belongs in the user's terminal rather than fighting its color scheme.

use crate::framebuffer::IndexedColor;
use crate::lsh::{HIGHLIGHT_KIND_COUNT, HIGHLIGHT_KIND_NAMES, HighlightKind};

/// Which color each highlight kind is drawn in. `None` means "leave the
/// default foreground alone", which is what plain text and the markup
/// attributes (bold, italic, ...) want.
#[derive(Clone, Copy)]
pub struct Theme {
    colors: [Option<IndexedColor>; HIGHLIGHT_KIND_COUNT],
}

impl Theme {
    /// Starts from `default` so that a partial user theme only has to name the
    /// kinds it actually wants to change.
    pub fn from_default() -> Self {
        *builtin("default").expect("the default theme must exist")
    }

    pub fn set(&mut self, kind: HighlightKind, color: Option<IndexedColor>) {
        self.colors[kind as usize] = color;
    }

    pub fn get(&self, kind: HighlightKind) -> Option<IndexedColor> {
        self.colors[kind as usize]
    }
}

/// Builds a theme from `(kind name, color name)` pairs.
///
/// Unnamed kinds fall back to no color. This is a macro rather than a function
/// so the tables below stay readable and are built at compile time.
macro_rules! theme {
    ($($kind:ident => $color:ident),* $(,)?) => {{
        let mut colors = [None; HIGHLIGHT_KIND_COUNT];
        $(colors[HighlightKind::$kind as usize] = Some(IndexedColor::$color);)*
        Theme { colors }
    }};
}

/// The themes that ship with the editor.
///
/// The first entry is the fallback and must be named `default`.
pub static BUILTIN: &[(&str, Theme)] = &[
    // Exactly what the editor used before themes existed.
    (
        "default",
        theme! {
            Comment => Green,
            Method => BrightYellow,
            String => BrightRed,
            Variable => BrightCyan,
            ConstantLanguage => BrightBlue,
            ConstantNumeric => BrightGreen,
            KeywordControl => BrightMagenta,
            KeywordOther => BrightBlue,
            KeywordPreprocessor => BrightBlue,
            MarkupChanged => BrightBlue,
            MarkupDeleted => BrightRed,
            MarkupHeading => BrightBlue,
            MarkupInserted => BrightGreen,
            MarkupList => BrightBlue,
            MetaHeader => BrightBlue,
            StorageAnnotation => Cyan,
            StorageType => Cyan,
        },
    ),
    // Leans on the bright half of the palette throughout.
    (
        "high-contrast",
        theme! {
            Comment => BrightGreen,
            Method => BrightYellow,
            String => BrightRed,
            Variable => BrightWhite,
            ConstantLanguage => BrightCyan,
            ConstantNumeric => BrightCyan,
            KeywordControl => BrightMagenta,
            KeywordOther => BrightMagenta,
            KeywordPreprocessor => BrightYellow,
            MarkupChanged => BrightYellow,
            MarkupDeleted => BrightRed,
            MarkupHeading => BrightWhite,
            MarkupInserted => BrightGreen,
            MarkupList => BrightCyan,
            MetaHeader => BrightWhite,
            StorageAnnotation => BrightBlue,
            StorageType => BrightBlue,
        },
    ),
    // After Visual FoxPro's editor: green comments, blue keywords, dark red
    // strings, red operators.
    //
    // VFP drew keywords in the dark `Blue`, which was fine against its white
    // background but is close to unreadable on the dark terminals most people
    // use now, so the keyword colors are the bright variants here. Everything
    // else keeps the original relationships.
    (
        "foxpro",
        theme! {
            Comment => Green,
            String => Red,
            ConstantNumeric => Red,
            KeywordControl => BrightBlue,
            KeywordOther => BrightBlue,
            KeywordPreprocessor => BrightMagenta,
            ConstantLanguage => BrightBlue,
            StorageType => BrightBlue,
            StorageAnnotation => BrightMagenta,
            Method => BrightCyan,
            MarkupChanged => BrightBlue,
            MarkupDeleted => Red,
            MarkupHeading => BrightBlue,
            MarkupInserted => Green,
            MarkupList => BrightBlue,
            MetaHeader => BrightBlue,
        },
    ),
    // The Borland/Turbo palette that Clipper work was written in: yellow body
    // text, white keywords, cyan strings and grey comments.
    (
        "clipper",
        theme! {
            Comment => BrightBlack,
            String => BrightCyan,
            ConstantNumeric => BrightGreen,
            KeywordControl => BrightWhite,
            KeywordOther => BrightWhite,
            KeywordPreprocessor => BrightMagenta,
            ConstantLanguage => BrightMagenta,
            StorageType => BrightWhite,
            StorageAnnotation => BrightMagenta,
            Method => BrightYellow,
            Variable => Yellow,
            MarkupChanged => BrightCyan,
            MarkupDeleted => BrightRed,
            MarkupHeading => BrightWhite,
            MarkupInserted => BrightGreen,
            MarkupList => BrightCyan,
            MetaHeader => BrightWhite,
        },
    ),
    // Only comments and literals are tinted; code stays the default color.
    (
        "muted",
        theme! {
            Comment => BrightBlack,
            String => Green,
            ConstantLanguage => Cyan,
            ConstantNumeric => Cyan,
            KeywordControl => Blue,
            KeywordOther => Blue,
            KeywordPreprocessor => BrightBlack,
            MarkupChanged => Blue,
            MarkupDeleted => Red,
            MarkupHeading => Blue,
            MarkupInserted => Green,
            MarkupList => Blue,
            MetaHeader => Blue,
            StorageAnnotation => BrightBlack,
            StorageType => Blue,
        },
    ),
];

pub fn builtin(name: &str) -> Option<&'static Theme> {
    BUILTIN.iter().find(|(n, _)| *n == name).map(|(_, t)| t)
}

pub fn builtin_names() -> impl Iterator<Item = &'static str> {
    BUILTIN.iter().map(|(name, _)| *name)
}

// This is a global for the same reason `unicode::setup_ambiguous_width` is:
// it's set once from the settings and read from deep inside the render path,
// and threading it through every call in between would make a mess of the
// signatures for no benefit.
static mut CURRENT: Option<Theme> = None;

/// Installs the theme used by all subsequent rendering.
pub fn setup(theme: Theme) {
    unsafe { CURRENT = Some(theme) };
}

/// The color for a highlight kind under the active theme.
#[inline]
pub fn color_for(kind: HighlightKind) -> Option<IndexedColor> {
    // SAFETY: Written once during startup (and by the theme picker, which also
    // runs on the main thread) before any rendering reads it.
    match unsafe { CURRENT } {
        Some(theme) => theme.get(kind),
        None => builtin("default").and_then(|t| t.get(kind)),
    }
}

/// Parses a palette color name as written in `settings.json`.
///
/// Accepts the same names VS Code's terminal settings use, case-insensitively.
pub fn parse_color(name: &str) -> Option<IndexedColor> {
    // `None` is how a theme says "don't color this at all".
    if name.eq_ignore_ascii_case("none") || name.is_empty() {
        return None;
    }

    const NAMES: [(&str, IndexedColor); 16] = [
        ("black", IndexedColor::Black),
        ("red", IndexedColor::Red),
        ("green", IndexedColor::Green),
        ("yellow", IndexedColor::Yellow),
        ("blue", IndexedColor::Blue),
        ("magenta", IndexedColor::Magenta),
        ("cyan", IndexedColor::Cyan),
        ("white", IndexedColor::White),
        ("brightblack", IndexedColor::BrightBlack),
        ("brightred", IndexedColor::BrightRed),
        ("brightgreen", IndexedColor::BrightGreen),
        ("brightyellow", IndexedColor::BrightYellow),
        ("brightblue", IndexedColor::BrightBlue),
        ("brightmagenta", IndexedColor::BrightMagenta),
        ("brightcyan", IndexedColor::BrightCyan),
        ("brightwhite", IndexedColor::BrightWhite),
    ];

    NAMES
        .iter()
        .find(|(n, _)| name.eq_ignore_ascii_case(n))
        .map(|(_, color)| *color)
}

/// Parses a highlight kind name, e.g. `keyword.control`.
pub fn parse_kind(name: &str) -> Option<HighlightKind> {
    let index = HIGHLIGHT_KIND_NAMES.iter().position(|n| *n == name)?;
    HighlightKind::try_from(index as u32).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_matches_the_previous_hardcoded_colors() {
        // These were the values baked into `TextBuffer::render` before themes
        // existed; changing them silently would be a regression.
        let theme = Theme::from_default();
        assert_eq!(theme.get(HighlightKind::Comment), Some(IndexedColor::Green));
        assert_eq!(theme.get(HighlightKind::String), Some(IndexedColor::BrightRed));
        assert_eq!(theme.get(HighlightKind::KeywordControl), Some(IndexedColor::BrightMagenta));
        assert_eq!(theme.get(HighlightKind::StorageType), Some(IndexedColor::Cyan));
        // Plain text and the pure-attribute markup kinds have no color.
        assert_eq!(theme.get(HighlightKind::Other), None);
        assert_eq!(theme.get(HighlightKind::MarkupBold), None);
    }

    #[test]
    fn every_builtin_is_reachable_by_name() {
        for name in builtin_names() {
            assert!(builtin(name).is_some(), "{name}");
        }
        assert!(builtin("default").is_some());
        assert!(builtin("no-such-theme").is_none());
    }

    #[test]
    fn color_names_round_trip() {
        assert_eq!(parse_color("green"), Some(IndexedColor::Green));
        assert_eq!(parse_color("brightBlue"), Some(IndexedColor::BrightBlue));
        assert_eq!(parse_color("BRIGHTWHITE"), Some(IndexedColor::BrightWhite));
        assert_eq!(parse_color("none"), None);
        assert_eq!(parse_color(""), None);
        // Unknown names are rejected rather than silently ignored, so that a
        // typo in settings.json is reported instead of quietly doing nothing.
        assert_eq!(parse_color("chartreuse"), None);
    }

    #[test]
    fn kind_names_match_the_generated_table() {
        assert_eq!(parse_kind("comment"), Some(HighlightKind::Comment));
        assert_eq!(parse_kind("keyword.control"), Some(HighlightKind::KeywordControl));
        assert_eq!(parse_kind("constant.numeric"), Some(HighlightKind::ConstantNumeric));
        assert_eq!(parse_kind("other"), Some(HighlightKind::Other));
        assert_eq!(parse_kind("keyword.nonexistent"), None);

        // Every generated name has to parse, or a theme couldn't address it.
        for name in HIGHLIGHT_KIND_NAMES {
            assert!(parse_kind(name).is_some(), "{name}");
        }
    }

    #[test]
    fn partial_themes_inherit_the_default() {
        let mut theme = Theme::from_default();
        theme.set(HighlightKind::Comment, Some(IndexedColor::BrightBlack));

        assert_eq!(theme.get(HighlightKind::Comment), Some(IndexedColor::BrightBlack));
        // Untouched kinds keep the default.
        assert_eq!(theme.get(HighlightKind::String), Some(IndexedColor::BrightRed));
    }
}
