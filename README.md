# ![Application Icon for Edit](./assets/edit.svg) Edit

A simple editor for simple needs.

This editor pays homage to the classic [MS-DOS Editor](https://en.wikipedia.org/wiki/MS-DOS_Editor), but with a modern interface and input controls similar to VS Code. The goal is to provide an accessible editor that even users largely unfamiliar with terminals can easily use.

![Screenshot of Edit with the About dialog in the foreground](./assets/edit_hero_image.png)

> [!NOTE]
> **This is a fork of [microsoft/edit](https://github.com/microsoft/edit).**
> It adds an embedded terminal panel, external file change detection, syntax color themes, and a few more language definitions.
> None of that is in the upstream releases, so the packages under [Installation](#installation) will **not** include these features — build from source instead.

## What this fork adds

### Embedded terminal panel

A terminal panel along the bottom of the window, so you can build, run tests, or talk to an interactive CLI without leaving the editor. It is a real pseudo console, not a captured-output pane: full-screen TUI applications work in it.

```
┌────────────────────────────────────────────────────────┐
│ File  Edit  View  Terminal  Help                       │
│  1 │ fn main() {                                       │
│  2 │     println!("hi");                               │
├─ 1: cmd.exe   [2: ✳ Claude Code] ──────────────────────┤
│ $ cargo run                                            │
│ hi                                                     │
├────────────────────────────────────────────────────────┤
│ Ln 2, Col 5              UTF-8              main.rs     │
└────────────────────────────────────────────────────────┘
```

The panel holds any number of terminals as tabs. Only the visible one is drawn, but all of them keep running and keep their output, so a build can finish in one tab while you work in another.

Key | Action
--- | ---
<kbd>F12</kbd> | Show or hide the panel
<kbd>Shift</kbd>+<kbd>F12</kbd> | Open another terminal tab
<kbd>Ctrl</kbd>+<kbd>F12</kbd> | Switch to the next tab
<kbd>F6</kbd> | Move focus between the editor and the panel
<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>↑</kbd>/<kbd>↓</kbd> or <kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>↑</kbd>/<kbd>↓</kbd> | Make the panel taller or shorter

Terminals disagree about which modified arrow keys they pass through, so the panel accepts two combinations for resizing and the **Terminal** menu carries the same commands for when neither arrives.

While the panel has focus, almost every key goes to the child process; the shortcuts above are the exceptions. Use <kbd>F6</kbd> to get back to the editor. The mouse wheel scrolls the terminal's scrollback unless the application asked for mouse reporting. When a child exits, its tab stays open so you can read what it printed; <kbd>Enter</kbd> closes it.

The same commands are available from the **Terminal** menu.

> [!IMPORTANT]
> The terminal panel is **Windows only** for now. It is built on ConPTY; the Unix side is stubbed out and reports "unsupported".

### External file change detection

Files opened in the editor are watched for modifications made by anything else — a build script, `git`, or an agent running in the terminal panel.

* A file with no unsaved changes is reloaded silently, keeping your cursor position.
* A file with unsaved changes never changes under you. A dialog shows what differs and offers **Reload** or **Keep Mine**.

```
┌─ File Changed ────────────────────────────────────┐
│           demo.md was modified by another program │
│      -  your version          +  on disk          │
│ -MYEDIT ## Build Instructions                     │
│ +## Build Instructions                            │
│  * Install Rust                                   │
│ -* Clone the repository                           │
│ +* Clone this fork:                               │
│ +  git clone https://github.com/akirajnn/edit.git │
│  * Run cargo build                                │
│ … 2 unchanged lines …                             │
│            [Reload]      [Keep Mine]              │
└───────────────────────────────────────────────────┘
```

In a conflict the diff is between *your* buffer and what's on disk, so `-` is what reloading would take away and `+` is what it would bring in — which is not the same as a `git diff`'s old and new. Long runs of unchanged lines are collapsed, and the colors follow the active theme.

Watching costs nothing while idle: a background thread does the polling and only wakes the editor when something actually changed.

### Syntax color themes

Highlighting colors are configurable. Pick one from **View → Color Theme** — moving through the list previews it immediately, <kbd>Enter</kbd> applies and remembers it, <kbd>Esc</kbd> restores what you had.

Themes map syntax to the terminal's own 16 palette colors rather than fixed RGB, so the editor keeps looking like it belongs in your terminal's color scheme.

Built-in themes:

Name | Description
--- | ---
`default` | The colors the editor has always used.
`high-contrast` | The bright half of the palette throughout.
`muted` | Only comments and literals are tinted; code stays the default color.
`foxpro` | After Visual FoxPro's editor: green comments, blue keywords, dark red strings.
`clipper` | The Borland/Turbo palette Clipper work was written in: yellow body text, white keywords, cyan strings, grey comments.

`foxpro` and `clipper` recreate the *syntax* colors of those environments. They can't reproduce the blue full-screen background those tools were known for: a theme only chooses colors for syntax, while the editor's own background comes from the terminal's palette.

### Markdown preview

<kbd>F7</kbd>, or **View → Markdown Preview**, renders the current buffer in a popup.

The editor doesn't render Markdown itself; it runs [`glow`](https://github.com/charmbracelet/glow) in a pseudo console and shows the result, so the preview is exactly what that tool produces — colors, styled headings and all. Any other renderer works just as well:

```jsonc
"markdown.previewCommand": "mdcat"
```

What gets previewed is the buffer, not the file on disk, so unsaved edits and untitled buffers preview fine. <kbd>F7</kbd> again closes it, and <kbd>↑</kbd>/<kbd>↓</kbd>, <kbd>PgUp</kbd>/<kbd>PgDn</kbd> and <kbd>Home</kbd>/<kbd>End</kbd> move through a long document.

> [!NOTE]
> The default command is `glow`, not `glow -p`. Glow's pager runs `less`, which Windows doesn't ship, so `-p` fails with `executable file not found in %PATH%`. Scrolling is handled by the editor instead.

`glow` isn't bundled. If it isn't installed the popup explains how to get it:

```powershell
winget install charmbracelet.glow
```

Make sure it ends up on your `PATH`, or point `markdown.previewCommand` at it directly.

### More language definitions

Added: **CSS**, **PHP**, and a dedicated **HTML** definition (previously `.html` fell back to the XML rules). `.htm` is recognised too.

The HTML definition hands `<script>` contents to the JavaScript rules and `<style>` contents to the CSS rules. The PHP definition highlights the surrounding markup as well as the code inside `<?php … ?>` and `<?= … ?>`.

## Configuration

Settings live in a JSON file that also accepts comments and trailing commas:

Platform | Path
--- | ---
Windows | `%APPDATA%\Microsoft\Edit\settings.json`
macOS | `~/Library/Application Support/com.microsoft.edit/settings.json`
Linux / other | `$XDG_CONFIG_HOME/msedit/settings.json` (or `~/.config/msedit/settings.json`)

Open it from **File → Preferences**.

```jsonc
{
  // A built-in theme, or a key from "themes" below.
  "theme": "high-contrast",

  // Custom themes start from "default", so you only list what you change.
  "themes": {
    "my-theme": {
      "comment":          "brightBlack",
      "string":           "green",
      "keyword.control":  "brightMagenta",
      "constant.numeric": "cyan"
    }
  },

  // Force a language for paths that aren't detected by extension.
  "files.associations": {
    "*.vue": "html"
  },

  // What each terminal tab runs. Defaults to %COMSPEC% (or $SHELL).
  "terminal.shell": "pwsh.exe -NoLogo",

  // Lines of scrollback each terminal keeps. Defaults to 10000.
  "terminal.scrollback": 5000,

  // What renders the Markdown preview. Defaults to "glow".
  "markdown.previewCommand": "glow -w 100 -s dark"
}
```

Theme colors are palette names: `black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `white`, their `bright…` counterparts, or `none` to leave a token uncolored. The syntax names on the left (`comment`, `keyword.control`, …) are the highlight kinds the language definitions emit; see [crates/lsh/definitions](crates/lsh/definitions).

The value of a file association is a language id, not a display name. The available ids are:
`c`, `cpp`, `csharp`, `css`, `diff`, `fsharp`, `git-commit`, `git-rebase`, `go`, `html`, `ignore`, `java`, `javascript`, `json`, `lsh`, `lua`, `markdown`, `odin`, `php`, `powershell`, `properties`, `python`, `ruby`, `rust`, `shellscript`, `toml`, `xml`, `yaml`.

## Known limitations

Terminal panel:

* Windows only (see above).
* Every tab runs the same `terminal.shell`; a tab can't be given its own command.
* A hidden tab isn't resized until you switch to it.

Syntax highlighting:

* Markdown only highlights fenced code for a fixed set of languages: `sh`/`bash`, `diff`, `javascript`/`js`, `json`, `odin`, `py`/`python`, `rs`/`rust`, `yaml` and `pwsh`/`powershell`.
* CSS treats the inside of a nested at-rule as declarations, so the selectors in `@media screen { .card { … } }` are colored as values rather than selectors. Top level rules are fine.
* PHP doesn't understand heredoc/nowdoc (`<<<EOT`); the body is left uncolored.
* HTML looks for `</script>` and `</style>` at the start of a line, so a one-line `<script>f()</script>` keeps script highlighting to the end of that line.

## Installation

> [!NOTE]
> These install **upstream** microsoft/edit, without the features described above. To get this fork, follow [Build Instructions](#build-instructions).

[![Packaging status](https://repology.org/badge/vertical-allrepos/microsoft-edit.svg?exclude_unsupported=1)](https://repology.org/project/microsoft-edit/versions)

You can also download binaries from [our Releases page](https://github.com/microsoft/edit/releases/latest).

### Windows

You can install the latest version with WinGet:
```powershell
winget install Microsoft.Edit
```

### Linux (build from source)

If your distribution does not provide binaries, or if you'd like to build your own, you can use our install script, provided you have installed:
* Rust (via `rustup` or similar)
* A C compiler (e.g. `gcc`)
* ICU (e.g. libicu78, libicu, icu)
* curl/wget and tar

The following command will then install `msedit` into `~/.local/bin`:
```sh
curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/microsoft/edit/main/assets/install.sh | sh
```

Additional flags are `--dev`, to build directly from the main branch, and `--system` to install into `/usr/local/bin`. For instance:
```sh
curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/microsoft/edit/main/assets/install.sh | sh -s -- --dev --system
```

### macOS

You can install the latest version with Homebrew:
```sh
brew install msedit
```

## Build Instructions

* [Install Rust](https://www.rust-lang.org/tools/install)
* Clone this fork:
  ```sh
  git clone https://github.com/akirajnn/edit.git
  cd edit
  git switch feat/terminal-themes-and-file-watch
  ```
  The `main` branch tracks upstream and has none of the additions above.
* If you're using nightly Rust:
  ```sh
  cargo build --release --config .cargo/release.toml
  ```
* If you're using stable Rust:
  * Ideally: Set the environment variable `RUSTC_BOOTSTRAP=1` and use the **nightly** build instructions above.
    This is recommended, because it drastically reduces the binary size and slightly improves performance.
  * Otherwise, simply run:
    ```sh
    cargo build --release
    ```

### Build Configuration

You can set the following environment variables at build-time to configure the build:

Environment variable | Description
--- | ---
`EDIT_CFG_ICU*` | See [ICU library name (SONAME)](#icu-library-name-soname) below for details. Linux package maintainers are advised to review and configure these options.
`EDIT_CFG_LANGUAGES` | A comma-separated list of languages to include in the build. See [i18n/edit.toml](i18n/edit.toml) for available languages.

### Debugging syntax highlighting

`pty_dump` runs a program under a pseudo console, feeds its output through the terminal emulator, and prints the resulting screen. It's the quickest way to see what a terminal application actually draws, including anything the emulator didn't understand:

```sh
cargo run --example pty_dump -- 5 cmd.exe
```

`lsh-bin` renders a file with the language definitions and prints it with ANSI colors, which is useful when writing or debugging a `.lsh` file:

```sh
cargo run -p lsh-bin -- render --input src/main.rs crates/lsh/definitions
```

## Notes to Package Maintainers

### Package Naming

The canonical executable name is "edit" and the alternative name is "msedit".
We're aware of the potential conflict of "edit" with existing commands and recommend alternatively naming packages and executables "msedit".
Names such as "ms-edit" should be avoided.
Assigning an "edit" alias is recommended, if possible.

### ICU library name (SONAME)

This project optionally depends on the ICU library for its Search and Replace functionality.

By default, the project will look for the following library names:

 Variable | Windows | macOS | Linux / Other
----------|---------|-------|---------------
`EDIT_CFG_ICUUC_SONAME` | `icuuc.dll` | `libicucore.dylib` | `libicuuc.so`
`EDIT_CFG_ICUI18N_SONAME` | `icuin.dll` | `libicucore.dylib` | `libicui18n.so`

If your installation uses a different SONAME, please set the following environment variable at build time:
* `EDIT_CFG_ICUUC_SONAME`:
  For instance, `libicuuc.so.76`.
* `EDIT_CFG_ICUI18N_SONAME`:
  For instance, `libicui18n.so.76`.

Additionally, this project assumes that the ICU exports symbols without `_` prefix and without version suffix, such as `u_errorName`.
If your installation uses versioned exports, please set:
* `EDIT_CFG_ICU_CPP_EXPORTS`:
  If set to `true`, it'll look for C++ symbols such as `_u_errorName`.
  Enabled by default on macOS.
* `EDIT_CFG_ICU_RENAMING_VERSION`:
  If set to a version number, such as `76`, it'll look for symbols such as `u_errorName_76`.

Finally, you can set the following environment variables:
* `EDIT_CFG_ICU_RENAMING_AUTO_DETECT`:
  If set to `true`, the executable will try to detect the `EDIT_CFG_ICU_RENAMING_VERSION` value at runtime.
  The way it does this is not officially supported by ICU and as such is not recommended to be relied upon.
  Enabled by default on UNIX (excluding macOS) if no other options are set.

To test your build settings, run `cargo test` with the `--ignored` flag. For instance:
```sh
cargo test -- --ignored
```
