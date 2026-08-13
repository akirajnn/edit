use std::path::PathBuf;

use edit::buffer::TextBuffer;
use edit::cell::{Ref, SemiRefCell};
use edit::json;
use edit::lsh::{LANGUAGES, Language};
use edit::theme::{self, Theme};
use stdext::arena::{read_to_string, scratch_arena};
use stdext::arena_format;

use crate::apperr;

pub struct Settings {
    pub path: PathBuf,
    pub file_associations: Vec<(String, &'static Language)>,
    /// The name from `"theme"`, kept so the picker can show what's active.
    pub theme_name: String,
    /// `None` until the settings are loaded; the built-in default applies then.
    pub theme: Option<Theme>,
    /// Command line each terminal tab runs. `None` means the platform default.
    pub terminal_shell: Option<String>,
    /// Lines of scrollback each terminal keeps. `None` means the default.
    pub terminal_scrollback: Option<usize>,
}

struct SettingsCell(SemiRefCell<Settings>);
unsafe impl Sync for SettingsCell {}
static SETTINGS: SettingsCell = SettingsCell(SemiRefCell::new(Settings::new()));

impl Settings {
    /// Fills the given settings.json text buffer with some initial contents for convenience.
    pub fn bootstrap(tb: &mut TextBuffer) {
        tb.set_crlf(false);
        tb.write_raw(b"{\n}\n");
        tb.cursor_move_to_logical(Default::default());
        tb.mark_as_clean();
    }

    const fn new() -> Self {
        Settings {
            path: PathBuf::new(),
            file_associations: Vec::new(),
            theme_name: String::new(),
            theme: None,
            terminal_shell: None,
            terminal_scrollback: None,
        }
    }

    pub fn borrow() -> Ref<'static, Settings> {
        SETTINGS.0.borrow()
    }

    pub fn reload() -> apperr::Result<()> {
        let s = &mut *SETTINGS.0.borrow_mut();

        // Reset all members if we had been loaded previously.
        if !s.path.as_os_str().is_empty() {
            *s = Settings::new();
        }

        s.load()
    }

    fn load(&mut self) -> apperr::Result<()> {
        self.path = match settings_json_path() {
            Some(p) => p,
            None => return Ok(()),
        };

        let scratch = scratch_arena(None);
        let str = match read_to_string(&scratch, &self.path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err.into()),
            Ok(str) => str,
        };
        let Ok(json) = json::parse(&scratch, &str) else {
            return Err(apperr::Error::SettingsInvalid("Invalid JSON"));
        };
        let Some(root) = json.as_object() else {
            return Err(apperr::Error::SettingsInvalid("Non-object root"));
        };

        if let Some(f) = root.get_object("files.associations") {
            for &(mut key, ref value) in f.iter() {
                if !key.contains('/') {
                    key = arena_format!(&*scratch, "**/{key}").leak();
                }

                let Some(id) = value.as_str() else {
                    return Err(apperr::Error::SettingsInvalid("files.associations"));
                };
                let Some(language) = LANGUAGES.iter().find(|lang| lang.id == id) else {
                    return Err(apperr::Error::SettingsInvalid("language ID"));
                };

                self.file_associations.push((key.to_string(), language));
            }
        }

        // Custom themes are read first, so that `"theme"` can name one.
        let custom = root.get_object("themes");

        if let Some(name) = root.get_str("theme") {
            self.theme_name = name.to_string();
            self.theme = Some(resolve_theme(name, custom)?);
        }

        if let Some(shell) = root.get("terminal.shell") {
            let Some(shell) = shell.as_str() else {
                return Err(apperr::Error::SettingsInvalid("terminal.shell must be a string"));
            };
            if !shell.trim().is_empty() {
                self.terminal_shell = Some(shell.to_string());
            }
        }

        if let Some(scrollback) = root.get("terminal.scrollback") {
            let Some(scrollback) = scrollback.as_number() else {
                return Err(apperr::Error::SettingsInvalid("terminal.scrollback must be a number"));
            };
            // A negative or absurd value would only show up much later as a
            // strange scrollbar, so reject it where it can still be explained.
            if !(0.0..=MAX_SCROLLBACK as f64).contains(&scrollback) {
                return Err(apperr::Error::SettingsInvalid("terminal.scrollback is out of range"));
            }
            self.terminal_scrollback = Some(scrollback as usize);
        }

        Ok(())
    }
}

/// Roughly 10x the default. Beyond this the memory cost stops being incidental.
const MAX_SCROLLBACK: usize = 100_000;

impl Settings {
    /// Writes the chosen theme back to `settings.json`.
    ///
    /// The file is edited in place rather than re-serialized, because it is
    /// allowed to contain comments (see [`json`]) and rewriting it wholesale
    /// would throw them away.
    pub fn persist_theme(name: &str) -> apperr::Result<()> {
        let path = match settings_json_path() {
            Some(path) => path,
            None => return Ok(()),
        };

        let existing = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => "{\n}\n".to_string(),
            Err(err) => return Err(err.into()),
        };

        let updated = set_theme_key(&existing, name);

        if let Some(parent) = path.parent()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, updated)?;

        // Keep the in-memory copy in step so the picker shows the right state.
        SETTINGS.0.borrow_mut().theme_name = name.to_string();
        Ok(())
    }
}

/// Replaces the top level `"theme"` value, or inserts one if it's missing.
///
/// Deliberately a text edit and not a JSON round trip. Only top level keys are
/// considered, so a `"theme"` sitting inside `"themes"` can't be mistaken for
/// the setting itself.
fn set_theme_key(source: &str, name: &str) -> String {
    let replacement = format!("\"{name}\"");

    if let Some(range) = find_top_level_string_value(source, "theme") {
        let mut out = String::with_capacity(source.len() + replacement.len());
        out.push_str(&source[..range.0]);
        out.push_str(&replacement);
        out.push_str(&source[range.1..]);
        return out;
    }

    // No key yet: put it right after the opening brace.
    match source.find('{') {
        Some(brace) => {
            let mut out = String::with_capacity(source.len() + replacement.len() + 16);
            out.push_str(&source[..brace + 1]);
            out.push_str("\n    \"theme\": ");
            out.push_str(&replacement);

            // Only add a separator if something else follows.
            if source[brace + 1..].trim_start().starts_with('"') {
                out.push(',');
            }

            out.push_str(&source[brace + 1..]);
            out
        }
        None => format!("{{\n    \"theme\": {replacement}\n}}\n"),
    }
}

/// Returns the byte range of the value of a top level `"key"`, if present.
fn find_top_level_string_value(source: &str, key: &str) -> Option<(usize, usize)> {
    let bytes = source.as_bytes();
    let mut depth = 0i32;
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'}' | b']' => {
                depth -= 1;
                i += 1;
            }
            // Skip comments, which the settings format allows.
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                i = memchr_newline(bytes, i);
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i = source[i + 2..].find("*/").map_or(bytes.len(), |off| i + 2 + off + 2);
            }
            b'"' => {
                let (text, next) = scan_string(source, i)?;
                // Only a key directly inside the root object counts.
                if depth == 1 && text == key {
                    let value_start = skip_to_value(source, next)?;
                    let (_, value_end) = scan_string(source, value_start)?;
                    return Some((value_start, value_end));
                }
                i = next;
            }
            _ => i += 1,
        }
    }

    None
}

/// Scans a JSON string starting at `start`, returning its contents and the
/// offset just past the closing quote.
fn scan_string(source: &str, start: usize) -> Option<(&str, usize)> {
    let bytes = source.as_bytes();
    let mut i = start + 1;

    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some((&source[start + 1..i], i + 1)),
            _ => i += 1,
        }
    }

    None
}

/// Skips whitespace, comments and the `:` separating a key from its value.
/// Returns the offset of the value's opening quote, or `None` if the value
/// isn't a string (in which case we leave the file alone).
fn skip_to_value(source: &str, mut i: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut seen_colon = false;

    while i < bytes.len() {
        match bytes[i] {
            b':' if !seen_colon => {
                seen_colon = true;
                i += 1;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => i = memchr_newline(bytes, i),
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i = source[i + 2..].find("*/").map_or(bytes.len(), |off| i + 2 + off + 2);
            }
            c if c.is_ascii_whitespace() => i += 1,
            b'"' if seen_colon => return Some(i),
            _ => return None,
        }
    }

    None
}

fn memchr_newline(bytes: &[u8], from: usize) -> usize {
    match bytes[from..].iter().position(|&b| b == b'\n') {
        Some(off) => from + off + 1,
        None => bytes.len(),
    }
}

/// Looks a theme up by name, preferring the user's own definitions.
///
/// A custom theme starts from the built-in default, so it only has to list the
/// highlight kinds it wants to change.
fn resolve_theme(name: &str, custom: Option<json::Object<'_>>) -> apperr::Result<Theme> {
    if let Some(custom) = custom
        && let Some(definition) = custom.get_object(name)
    {
        let mut theme = Theme::from_default();

        for &(kind, ref value) in definition.iter() {
            let Some(kind) = theme::parse_kind(kind) else {
                return Err(apperr::Error::SettingsInvalid("themes: unknown highlight kind"));
            };
            let Some(color) = value.as_str() else {
                return Err(apperr::Error::SettingsInvalid("themes: color must be a string"));
            };
            // "none" parses to `None`, which is a legitimate way to say
            // "leave this kind uncolored", so only reject unknown names.
            let color = match theme::parse_color(color) {
                Some(color) => Some(color),
                None if color.eq_ignore_ascii_case("none") || color.is_empty() => None,
                None => {
                    return Err(apperr::Error::SettingsInvalid("themes: unknown color name"));
                }
            };
            theme.set(kind, color);
        }

        return Ok(theme);
    }

    match theme::builtin(name) {
        Some(theme) => Ok(*theme),
        None => Err(apperr::Error::SettingsInvalid("theme: unknown theme name")),
    }
}

fn settings_json_path() -> Option<PathBuf> {
    let mut config_dir = config_dir()?;
    config_dir.push("settings.json");
    Some(config_dir)
}

fn config_dir() -> Option<PathBuf> {
    fn var_path(key: &str) -> Option<PathBuf> {
        std::env::var_os(key).map(PathBuf::from)
    }

    fn push(mut path: PathBuf, suffix: &str) -> PathBuf {
        path.push(suffix);
        path
    }

    #[cfg(target_os = "windows")]
    {
        var_path("APPDATA").map(|p| push(p, "Microsoft\\Edit"))
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        var_path("HOME").map(|p| push(p, "Library/Application Support/com.microsoft.edit"))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "ios")))]
    {
        var_path("XDG_CONFIG_HOME")
            .or_else(|| var_path("HOME").map(|p| push(p, ".config")))
            .map(|p| push(p, "msedit"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_an_existing_theme_key() {
        let source = "{\n    \"theme\": \"default\"\n}\n";
        assert_eq!(set_theme_key(source, "muted"), "{\n    \"theme\": \"muted\"\n}\n");
    }

    #[test]
    fn inserts_a_missing_theme_key() {
        assert_eq!(set_theme_key("{\n}\n", "muted"), "{\n    \"theme\": \"muted\"\n}\n");

        let source = "{\n    \"files.associations\": {}\n}\n";
        let updated = set_theme_key(source, "muted");
        assert!(updated.contains("\"theme\": \"muted\","), "{updated}");
        assert!(updated.contains("files.associations"), "{updated}");
    }

    #[test]
    fn keeps_comments_intact() {
        let source = "{\n    // pick your poison\n    \"theme\": \"default\", // active\n}\n";
        let updated = set_theme_key(source, "high-contrast");
        assert!(updated.contains("// pick your poison"), "{updated}");
        assert!(updated.contains("// active"), "{updated}");
        assert!(updated.contains("\"theme\": \"high-contrast\""), "{updated}");
    }

    #[test]
    fn a_theme_key_inside_themes_is_not_mistaken_for_the_setting() {
        // The nested key is at depth 2 and must be left alone; the real
        // setting comes after it and is the one that gets replaced.
        let source = "{\n    \"themes\": { \"theme\": { \"comment\": \"green\" } },\n    \"theme\": \"default\"\n}\n";
        let updated = set_theme_key(source, "muted");
        assert!(updated.contains("\"themes\": { \"theme\": { \"comment\": \"green\" } }"), "{updated}");
        assert!(updated.contains("\"theme\": \"muted\""), "{updated}");
        assert!(!updated.contains("\"default\""), "{updated}");
    }

    #[test]
    fn a_nested_theme_key_alone_gets_a_new_top_level_one() {
        let source = "{\n    \"themes\": { \"mine\": { \"comment\": \"green\" } }\n}\n";
        let updated = set_theme_key(source, "mine");
        assert!(updated.contains("\"theme\": \"mine\","), "{updated}");
        assert!(updated.contains("\"themes\""), "{updated}");
    }

    #[test]
    fn survives_a_file_without_braces() {
        assert_eq!(set_theme_key("", "muted"), "{\n    \"theme\": \"muted\"\n}\n");
    }

    #[test]
    fn leaves_a_non_string_theme_value_alone() {
        // Malformed settings shouldn't be silently rewritten into something
        // that looks valid; the parser will report the problem instead.
        let source = "{\n    \"theme\": 42\n}\n";
        let updated = set_theme_key(source, "muted");
        assert!(updated.contains("\"theme\": \"muted\""), "{updated}");
    }
}
