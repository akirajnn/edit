# ![Application Icon for Edit](./assets/edit.svg) Edit

A simple editor for simple needs.

This editor pays homage to the classic [MS-DOS Editor](https://en.wikipedia.org/wiki/MS-DOS_Editor), but with a modern interface and input controls similar to VS Code. The goal is to provide an accessible editor that even users largely unfamiliar with terminals can easily use.

![Screenshot of Edit with the About dialog in the foreground](./assets/edit_hero_image.png)

> [!NOTE]
> **This is a fork of [microsoft/edit](https://github.com/microsoft/edit).**
> Everything described below is added here and is not in the upstream releases, so the packages under [Installation](#installation) will **not** include it — build from source instead.

## Why this fork exists

I wanted to write .NET Core from the console — vibe coding, with an AI CLI sitting right next to the code instead of in another window — and to have that be fast, friendly and convenient rather than something I put up with.

[microsoft/edit](https://github.com/microsoft/edit) is what made that look possible. It's the MS-DOS Editor rebuilt in Rust, and using it was a genuine surprise: it starts instantly, it stays out of the way, and it's the first editor in a long time that felt properly at home in a console. It seemed like the right thing to build on.

So this fork is an attempt to grow it into something you can actually develop in — a light console IDE for .NET Core, TypeScript, or whatever I happen to be working in. Everything here started as something I wanted while working, not as a feature list: a terminal panel so a build and an agent can run beside the file I'm editing, detection of files changed behind my back, a Markdown preview, syntax colors I can live with. It's a personal tool first, and I use it every day.

The terminal panel started out Windows-only, built on ConPTY. It now also runs on Linux and macOS via the POSIX PTY API — CI covers Windows and Linux; macOS shares the same Unix code path but isn't CI-verified there yet.

## What this fork adds

### Embedded terminal panel

A terminal panel along the bottom of the window, so you can build, run tests, or talk to an interactive CLI without leaving the editor. It is a real pseudo console, not a captured-output pane: full-screen TUI applications work in it.

```
┌────────────────────────────────────────────────────────┐
│ File  Edit  View  Terminal  Help                       │
├────────────────────────────────────────────────────────┤
│  1 │ fn main() {                                       │
│  2 │     println!("hi");                               │
│  3 │                                                   │
│  4 │                                                   │
├─ 1: cmd.exe   [2: ✳ Claude Code] ──────────────────────┤
│ $ cargo run                                            │
│ hi                                                     │
│                                                        │
├────────────────────────────────────────────────────────┤
│ Ln 2, Col 5              UTF-8              main.rs    │
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
<kbd>Shift</kbd>+<kbd>PgUp</kbd>/<kbd>PgDn</kbd> | Scroll back through the output
<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Home</kbd>/<kbd>End</kbd> | Jump to the top or bottom of the scrollback
<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> | Copy the selected text

Terminals disagree about which modified arrow keys they pass through, so the panel accepts two combinations for resizing and the **Terminal** menu carries the same commands for when neither arrives.

While the panel has focus, almost every key goes to the child process; the shortcuts above are the exceptions. Use <kbd>F6</kbd> to get back to the editor.

**Scrolling and selecting don't need the focus.** The mouse wheel scrolls whatever it is over, so you can read back through a build's output without leaving the file you are editing. Dragging selects text.

To copy it, **right-click**, the way a Windows console has always worked. <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> does the same, and so does **Terminal → Copy Selection** — worth knowing, because some host terminals keep the right button for their own context menu and never pass the click on. `edit --probe-keys` reports mouse clicks as well as keys, so you can see in a second whether yours does.

Plain <kbd>Ctrl</kbd>+<kbd>C</kbd> is not copy and cannot be: it has to stay the child's interrupt, which is the whole reason terminals put copy on the shifted key. Right-click is copy-only here; pasting stays on <kbd>Ctrl</kbd>+<kbd>V</kbd>, since an accidental right-click paste into a live shell can run whatever was on the clipboard.

A selection stays on the text it was made on: scrolling the view doesn't move it, and neither does the program printing more output. It is dropped when the panel is resized or the program switches to a full-screen view, since the text genuinely moves then.

Both of these step aside for an application that asked for mouse reporting — inside `vim` or `htop`, the wheel and the pointer belong to it.

When a child exits its tab stays open so you can read what it printed, and since there is no longer anything to type at, the unshifted <kbd>↑</kbd>/<kbd>↓</kbd>, <kbd>PgUp</kbd>/<kbd>PgDn</kbd> and <kbd>Home</kbd>/<kbd>End</kbd> scroll too. <kbd>Enter</kbd> closes the tab.

The same commands are available from the **Terminal** menu.

The terminal panel works on **Windows (x64 and ARM64), macOS, and Linux**. On Windows it uses ConPTY; on macOS and Linux it uses the POSIX PTY API (`posix_openpt` / `fork` / `exec`).

### Word completion

<kbd>Alt</kbd>+<kbd>N</kbd> completes the word you are typing from the words already in the file.

```
┌────────────────────────────────────────────────────────┐
│  1 │ let price_before_tax = 10.0;                      │
│  2 │ let price_after_tax = price_bef                   │
│  3 │                      ┌──────────────────┐         │
│  4 │                      │ price_before_tax │         │
│  5 │                      │ price_after_tax  │         │
│    │                      └──────────────────┘         │
└────────────────────────────────────────────────────────┘
```

No language server, no subprocess, nothing to install. The names you want to type are nearly always ones the file already contains, and matching is the same fuzzy scoring the file picker uses, so `pbt` finds `price_before_tax`.

Key | Action
--- | ---
<kbd>Alt</kbd>+<kbd>N</kbd> | Open the list, then step down it
<kbd>Alt</kbd>+<kbd>P</kbd> | Open it on the *last* entry, then step up
<kbd>↑</kbd> / <kbd>↓</kbd> | Also move through it
<kbd>Enter</kbd> or <kbd>Tab</kbd> | Take the highlighted word
<kbd>Esc</kbd> | Dismiss

One pair of keys both opens the list and walks through it, so the fingers never move to the arrow keys — press <kbd>Alt</kbd>+<kbd>N</kbd> once for the list and again for the next entry. <kbd>Alt</kbd>+<kbd>P</kbd> opens on the last entry, so either end is one keystroke away.

The list never appears on its own, so it stays out of the way of ordinary typing. While it is open it claims only those keys — everything else goes to the buffer as usual, and the list narrows to whatever you have typed. Move off the word and it closes.

**If those keys are taken, change them.** `Ctrl+Space` would be the conventional choice and is exactly what Microsoft's IME uses to switch languages, which is why it isn't the default:

```jsonc
"completion.next": "ctrl+e",
"completion.prev": "ctrl+d"
```

Letters, digits, function keys and named keys (`space`, `tab`, `home`, `pageup`, …) can be bound, with any of `ctrl`, `alt` and `shift`. Punctuation cannot: a terminal has no way to send most <kbd>Ctrl</kbd>+punctuation combinations, so binding one would give you a shortcut that silently never fires — the editor refuses it rather than accepting it and doing nothing. An empty string switches a binding off.

To find out what your terminal and your IME actually leave available:

```
edit --probe-keys
```

Press anything and it prints the name to put in `settings.json`, or tells you the key never arrived at all.

Beyond the file's own words, you can add anything else worth offering — language keywords, names your project uses constantly — per language id:

```jsonc
"completion.keywords": {
  "rust": ["fn", "let", "mut", "pub", "impl", "match", "struct", "enum", "trait", "async", "await"],
  "csharp": ["public", "private", "static", "class", "var", "async", "await", "namespace"]
}
```

The ids are the same ones `files.associations` uses. These aren't taken from the syntax definitions on purpose: the keywords there live inside regular expressions, so extracting them would break whenever a definition is edited — and a hand-written list can hold your own vocabulary as well as the language's.

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
  "markdown.previewCommand": "glow -w 100 -s dark",

  // Extra completion candidates, on top of the words already in the file.
  "completion.keywords": {
    "rust": ["fn", "let", "mut", "pub", "impl", "match", "struct", "enum"]
  },

  // Which keys open and walk the completion list. Run `edit --probe-keys`
  // to see what your terminal and IME leave available.
  "completion.next": "alt+n",
  "completion.prev": "alt+p"
}
```

Theme colors are palette names: `black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `white`, their `bright…` counterparts, or `none` to leave a token uncolored. The syntax names on the left (`comment`, `keyword.control`, …) are the highlight kinds the language definitions emit; see [crates/lsh/definitions](crates/lsh/definitions).

The value of a file association is a language id, not a display name. The available ids are:
`c`, `cpp`, `csharp`, `css`, `diff`, `fsharp`, `git-commit`, `git-rebase`, `go`, `html`, `ignore`, `java`, `javascript`, `json`, `lsh`, `lua`, `markdown`, `odin`, `php`, `powershell`, `properties`, `python`, `ruby`, `rust`, `shellscript`, `toml`, `xml`, `yaml`.

## Known limitations

Terminal panel:

* Every tab runs the same `terminal.shell`; a tab can't be given its own command.
* A hidden tab isn't resized until you switch to it.
* The command string is split on whitespace to build the argument vector, so arguments that contain spaces must be passed through a shell invocation (e.g. `bash -c 'my command'` won't work as-is; use a wrapper script instead).

Markdown preview:

* Needs an external renderer; `glow` is not bundled.
* It renders once. Editing the buffer doesn't update an open preview — close and reopen it with <kbd>F7</kbd> twice.

Word completion:

* Candidates come from the file you are looking at, not from other open files or from anywhere on disk. It completes names, it doesn't know what they mean.
* The candidate list is gathered when you open it, so words typed elsewhere in the file since then only appear next time.
* Punctuation can't be bound as a shortcut — see above for why.

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

Windows on ARM (ARM64) is supported. To build this fork natively on an ARM64
device, follow the [Build Instructions](#build-instructions) and make sure the
ARM64 MSVC tools are installed as described in [Requirements](#requirements).

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

## Requirements

Software | Needed for | Install
--- | --- | ---
[Rust](https://www.rust-lang.org/tools/install) 1.93 or newer | Building. Older toolchains fail on the 2024 edition features this uses. | `winget install Rustlang.Rustup`
Visual Studio C++ build tools | Linking on Windows x64 and ARM64. Install the MSVC tools that match your Rust target. | `winget install Microsoft.VisualStudio.2022.BuildTools`
[git](https://git-scm.com/downloads) | Cloning this fork. | `winget install Git.Git`
[glow](https://github.com/charmbracelet/glow) | The Markdown preview (<kbd>F7</kbd>). **Optional** — everything else works without it, and the preview tells you if it's missing. | `winget install charmbracelet.glow`

The terminal panel needs nothing extra on any platform: it's built on ConPTY on Windows, and on the POSIX PTY API (part of libc) on macOS and Linux. On Linux you additionally need a C compiler, and ICU if you want Search and Replace — see [Notes to Package Maintainers](#icu-library-name-soname).

> [!TIP]
> After installing anything with WinGet, open a **new** terminal before checking. `PATH` changes don't reach shells that are already running, which looks exactly like the install having failed.

> [!NOTE]
> On Windows ARM64, the Build Tools installer must include the
> `Microsoft.VisualStudio.Component.VC.Tools.ARM64` component. If `cargo build`
> reports that `link.exe` is missing, add the ARM64 C++ build tools with the
> Visual Studio Installer and then build from a Developer Command Prompt.

## Build Instructions

* Clone this fork:
  ```sh
  git clone https://github.com/akirajnn/edit.git
  cd edit
  ```
  That puts you on `RUST_dotnet_core_IDE`, the default branch and the one everything above describes. The `main` branch tracks upstream and has none of it.
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

### What the binary needs at run time

Almost nothing. Editing, syntax highlighting, themes, word completion, the terminal panel and file change detection all work with no external anything.

The exceptions, all of which degrade rather than fail:

Wanted by | Needed for | Without it
--- | --- | ---
`icu.dll` / `libicuuc` | Find and Replace | Search is disabled; everything else is unaffected. Fuzzy matching falls back to ASCII case folding, so completion and the file picker keep working. Windows 10 1703 and later ship ICU with the OS.
`glow` | <kbd>F7</kbd> Markdown preview | A dialog says it isn't installed. Any other renderer works via `markdown.previewCommand`.
A shell | The terminal panel | Not a real concern; `%COMSPEC%` and `$SHELL` always exist.

None of these are loaded until something asks for them, so they cost nothing if you never use the feature.

### A single self-contained executable

The recommended build above already produces one. `.cargo/release.toml` links the MSVC runtime statically while keeping the UCRT dynamic, which is what removes the dependency on `VCRUNTIME140.dll` -- an optional Windows component that arrives with the Visual C++ Redistributable and so cannot be assumed on a machine you are copying a binary to.

A plain `cargo build --release` does *not* do this. Measured on x64:

Build | Size | Needs `VCRUNTIME140.dll`
--- | --- | ---
`cargo build --release` | 561 KB | yes
`--config .cargo/release.toml` | 583 KB | no

What is left imports only `KERNEL32.dll`, `ntdll.dll` and the `api-ms-win-*` stubs, all of which are part of Windows 10 and later.

The nightly path additionally wants the standard library source, since it rebuilds it:

```sh
rustup component add rust-src
```

Without it the build stops with *"library/Cargo.lock does not exist, unable to build with the standard library"*.

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
