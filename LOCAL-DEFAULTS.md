# Built-in defaults

For the complete setup on another Apple Silicon Mac (tmux, zsh prompt, mouse
selection and status badges included), use
[`make install-terminal`](TERMINAL-SETUP.md). Building only the app, as described
below, provides the embedded emulator defaults but not the complete profile.

This fork compiles our Alacritty preferences into the executable. Installing
the macOS app bundle provides the theme without an `alacritty.toml` file.
The default Menlo font is supplied by macOS.

## Included

- Merino Light colors, a blinking block cursor, and 12-point text.
- Window padding of 12 × 24, dynamic padding, and maximized startup.
- On macOS, a transparent title bar and both Option keys acting as Alt.
  Double-clicking its background follows the system title-bar action (Zoom,
  Fill, Minimize, or None), without changing the transparent appearance.
- On macOS, Cmd+Shift+L globally shows/hides Alacritty without Hammerspoon.
  Summoning restores the last used window, including a minimized window.
- 100,000 lines of history, selection copied to the clipboard, and the mouse
  pointer hidden while typing.
- On macOS, Menlo Regular, Bold, Italic, and Bold Italic with natural line
  height, matching Rex Beta’s default typography. Native decorations use Light.
- The previously bundled JetBrainsMono Nerd Font files remain available for
  explicit font overrides.
- On macOS, Cmd+Shift+[ / ] (including the { / } logical keys) and Ctrl+Tab /
  Ctrl+Shift+Tab select native tabs; Ctrl+Cmd+F toggles maximization.
  Alacritty's existing Cmd+T, Cmd+1–9, and font-size shortcuts still apply.
- On macOS, Shift+Enter sends Ctrl+J (LF) in legacy input mode, so Codex can
  insert a newline through tmux without enhanced keyboard negotiation. Plain
  Enter still sends CR. Apps using the kitty keyboard protocol receive native
  Shift+Enter encoding, and Alacritty search/vi mode retain their own bindings.

- OSC 7501 revision 0.4 support, enabled without configuration. Programs can
  report structured state/progress; native titles retain their normal title
  and append a bounded status label. Updates are coalesced and capped at 10 Hz.
  See [protocol behavior, limits, benchmarks and tmux support](OSC7501.md).

The Baidu Pinyin Shift/Enter fix remains pinned to the same patched winit.

## Pasting images on macOS

Copy a screenshot with Ctrl+Shift+Cmd+4 (or copy an image in Preview), then press
Cmd+V in Alacritty. When the clipboard has no text, the Paste action exports PNG,
TIFF, or JPEG image data as a PNG file and pastes its absolute path plus a space.
CLI tools that accept local image paths can then read the image. Ordinary text
paste keeps priority, and terminal search and OSC 52 clipboard queries stay
text-only. Custom bindings to `Paste` use the same behavior.

Files are named `alacritty-clipboard-*.png` in the macOS temporary directory
(`$TMPDIR`), with owner-only permissions. They remain after Alacritty exits so
tools can read them later; macOS manages temporary-file cleanup, and you can
delete these files when no longer needed. Paths with shell metacharacters are
quoted. The clipboard itself is unchanged.

This passes a local file path, not an upload: local tmux panes can access it,
while SSH sessions and containers need the file transferred or mounted first.
The receiving CLI must support image paths; Alacritty does not attach images
to arbitrary programs or render an inline image preview.

Native format-conversion tests use isolated pasteboards and require a logged-in
macOS desktop session: `cargo test -p alacritty macos::clipboard -- --ignored`.
They do not modify the system clipboard.

## Native macOS global hotkey

Alacritty registers Cmd+Shift+L with macOS while it is running. Press it in
another application to show and focus Alacritty; press it with Alacritty active
to hide the application. Hidden sessions continue running. This does not launch
Alacritty after it has quit. In `--daemon` mode it creates a window when needed.

Remove or disable your Hammerspoon binding for the same shortcut to avoid a
registration conflict. The built-in shortcut needs no keyboard monitoring or
Accessibility permission. Registration errors appear in the log; normal terminal
operation continues. This feature does not install a login item or change Spaces
or floating-window behavior.

Override or disable it in `alacritty.toml`:

```toml
[keyboard.global_hotkey]
enabled = true # false disables the global shortcut
key = "KeyL" # physical winit KeyCode, e.g. KeyT, Space, F12
mods = "Command|Shift" # also supports Control and Alt/Option
```

The shortcut follows physical key positions even with a non-Latin input method.
At least one modifier is required. Configuration reload unregisters an old
shortcut only after its replacement registers successfully. If another app owns
the new combination, the previous shortcut remains active. CLI overrides are
supported; per-window IPC overrides do not change application-wide registration.

## Local file links on macOS

The default mouse hints also recognize local paths containing a slash, including
`.codex-artifacts/project/index.html`, `./`, `../`, `~/`, and absolute paths.
Click to open an existing file or directory with its default macOS application.
When tmux or another program captures the mouse, hold **Cmd** or **Shift** while
clicking. Both modifiers keep clicks and selection drags in Alacritty.
URL and OSC 8 hyperlinks retain priority over plain-text file paths.

Relative paths use the foreground process's current directory. In tmux they use
the active pane of the client attached to that Alacritty window, including clients
started with `-L` or `-S`. Select the relevant pane before opening its links.
Paths in old scrollback are resolved against the current directory at click time.
Plain-text recognition excludes whitespace and surrounding Markdown punctuation;
paths containing spaces should be emitted as `file://` OSC 8 links instead.
An explicit `hints.enabled` configuration replaces these built-in hints.

The regression suite covers path recognition and resolution. To also check real
tmux clients, run `extra/path-hint-acceptance.py <alacritty-test-binary> <tmux>`
after `cargo test`, before cleaning the test target directory. It creates and
removes isolated tmux servers; it does not attach to existing user sessions.

## Configuration precedence

The order is built-in settings, imported configuration files, the main user
configuration, then CLI overrides. Reloading the user configuration uses the
same defaults. Settings remain in
[`defaults.toml`](alacritty/src/config/defaults.toml) and
[`defaults-macos.toml`](alacritty/src/config/defaults-macos.toml), included with
Rust's `include_str!`; no file is copied to a user's home directory at startup.

Key bindings stay in Rust so an explicit user binding replaces its matching
default, instead of running both actions. Existing tmux launch commands and
tmux key bindings continue to work when supplied by a user configuration.
This fork does not load tmux configuration or require tmux to start. Optional
tmux appearance settings are provided under `extra/`.

The built-in colors use the Merino Light source sRGB terminal palette from
Rex Beta's bundled `SuperUI_SuperDesignTokens.bundle/Contents/Resources/themes.json`
(`merino-light`): white background, dark text, and a blue cursor. Search, hints,
and the footer use matching palette accents. The previous Catppuccin license
notice remains in `LICENSE-CATPPUCCIN`.

## Matching Rex Beta on an existing installation

For the native macOS title-bar **◐** button and coordinated Merino Light / Dark
switching, use the [existing-machine upgrade](TERMINAL-SETUP.md#existing-dotfiles-based-machines).
Both palettes are bundled; there is no runtime Rex dependency. The button is
configured by `window.theme_toggle_command` and is hidden when that command is
absent. Its command runs asynchronously, with failures written to the app log.

Existing user imports override embedded defaults. A previous iTerm2 profile
can retain a different font, fixed line height, and a dark tmux tab bar even
when the terminal palette has changed. Apply the terminal font and tmux style
profiles together. Templates are [`extra/rex-appearance.toml`](extra/rex-appearance.toml),
[`extra/merino-light.toml`](extra/merino-light.toml), and
[`extra/merino-light-tmux.conf`](extra/merino-light-tmux.conf). Copy them into
`~/.config/alacritty/`, then import the appearance file followed by the color
file in the existing `alacritty.toml`:

```toml
[general]
import = [
  # Keep existing non-appearance imports before these entries.
  "~/.config/alacritty/rex-appearance.toml",
  "~/.config/alacritty/merino-light.toml",
]
```

These imports also work with an already-installed binary. Replace the old
iTerm2 appearance import, rather than stacking both font profiles; otherwise
its fixed line height remains in the merged configuration. Load the tmux
profile after shared bindings, and use the same profile when reloading tmux.

The local Rex Beta setup has font smoothing disabled. Alacritty’s CoreText rasterizer reads
the app-specific `AppleFontSmoothing` preference once per process. Use
`defaults write org.alacritty AppleFontSmoothing -int 0`, then relaunch Alacritty
to match that rendering; changing only the font does not clear this cache.
This preference does not change other applications.

Codex CLI caches its terminal foreground/background during startup. Sessions
started with the old dark palette can retain dark prompt backgrounds after
Alacritty reloads a light theme. Re-enter those Codex sessions after applying
the light terminal and tmux defaults. Existing tmux panes and session histories
can remain intact. Codex syntax highlighting has its own `/theme` picker.

## Fonts and packaging

The app template includes four unmodified fonts under
`extra/osx/Alacritty.app/Contents/Resources/fonts`, their upstream `OFL.txt`
and `README.md`, and SHA-256 checksums. They match the official
[Nerd Fonts v3.5.1 JetBrainsMono archive](https://github.com/ryanoasis/nerd-fonts/releases/download/v3.5.1/JetBrainsMono.tar.xz).

The template's `ATSApplicationFontsPath` activates these fonts only for the
bundled application, as documented in Apple's
[macOS Info.plist reference](https://developer.apple.com/library/archive/documentation/General/Reference/InfoPlistKeyReference/Articles/GeneralPurposeKeys.html#//apple_ref/doc/uid/TP40009254-SW8).
They are not installed into the system or user font directories. Activation
was checked with an isolated app using a temporary font-family name absent
from the machine's installed fonts, including matching a rendered font back
to a file inside that app bundle.

With Rust, Xcode Command Line Tools, and `scdoc` installed:

```sh
git clone https://github.com/shanghaikid/alacritty.git
cd alacritty
make app
ditto target/release/osx/Alacritty.app /Applications/Alacritty.app
```

Use `make app-universal` with both Rust macOS targets installed for a bundle
that supports Intel and Apple Silicon. The normal Makefile copies the full
app template, including its fonts and font registration key.

Running only `target/release/alacritty` retains the embedded settings and uses
macOS’s Menlo font. The `.app` bundle also includes the optional Nerd Fonts.
Other platforms retain their native default font families.
`TERM` retains upstream's automatic detection, using `xterm-256color` when
Alacritty's terminfo has not been separately installed on a fresh system.

## Rendering performance

Rendering snapshots share immutable cell extras and reuse their character buffer.
Default palette colors are resolved once per frame, including for syntax-highlighted
text. Complete viewport searches reuse matches after validating an exact snapshot of
characters and layout flags; pointer queries retain their lazy, early-exit scan.
Frame scheduling retains the backend's display-mode query, preserving fractional
refresh rates. `NSScreen.maximumFramesPerSecond` was benchmarked but rejected during
review because a screen's integer maximum is not an equivalent refresh-rate query.

See `extra/performance-results.txt` for measurements and reproducible checks. The
CPU benchmarks measure individual operations, not overall application speed.

## Codex selection and scrollback in tmux

Codex's alternate-screen transcript keeps its history inside the application.
Entering tmux copy mode freezes the visible screen; it cannot scroll that internal
history. Mouse-aware alternate-screen programs now receive ordinary clicks,
double/triple clicks, and Shift-clicks directly, so the application can keep its
selection attached to its own history. Main-screen shell output keeps the tmux
single-click anchor and Shift-click selection bindings. Option still forwards
mouse gestures to the application; explicit keyboard tmux copy mode is unchanged.

Do not enable Codex's `tui.raw_output_mode` and `tui.alternate_screen = "never"`
as a workaround here. The earlier experiment did not establish working resumed
scrollback and those overrides have been removed. The installed Codex 0.165.0
already supports selecting a character, scrolling while retaining that selection,
and Shift-clicking to extend it in its normal fullscreen transcript. A bare click
without a selected range is handled by Codex itself; tmux's empty-anchor behavior
only applies to tmux selections.

`python3 extra/test-codex-tmux-selection.py` exercises the installed Codex with
synthetic resumed history in an isolated tmux server and config directory. It
submits no prompt, uses no account, disables clipboard copying, and verifies
selection retention, Shift extension, composer input, and top status tabs. A
negative control reproduces the old unconditional click-to-copy-mode freeze.

Alacritty also consumes pixel scroll deltas in every gesture phase and retains
fractional rows across phase changes. macOS reports the start of momentum as a
new `Started` event, so discarding that event or clearing its remainder makes
trackpad scrolling lose motion at the transition.
The tmux selection configuration also keeps a 20 ms escape-sequence timeout so
fragmented mouse reports do not leak into the application's text input.

## Validation

```sh
cargo test --locked -p alacritty --bin alacritty config::
cargo build --locked --release --bin alacritty
python3 extra/hotkey-acceptance.py /path/to/Alacritty.app # macOS native integration
```

The configuration tests cover no user file, imports, partial overrides,
reload, CLI overrides, missing-file fallback, and replacement of matching
default key bindings.

The macOS hotkey integration test uses a disposable app and a temporary native
interposer. It verifies Carbon registration and callback dispatch, real window
visibility and minimized-window restore, held-key suppression, hot reload, stale
events, exclusive-registration conflicts, disable/reenable, and daemon window
creation. It injects Carbon events at the registered application target; physical
keyboard/WindowServer delivery should also be checked manually with Cmd+Shift+L.
Existing applications, Hammerspoon configuration, and terminal sessions are not
modified by the test.
