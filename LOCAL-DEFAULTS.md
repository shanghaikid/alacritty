# Built-in defaults

This fork compiles our Alacritty preferences into the executable. Installing
the macOS app bundle provides the theme and fonts without an `alacritty.toml`
file or a separate font installation.

## Included

- Catppuccin Mocha colors, a block cursor without blinking, and 11-point text.
- Window padding of 12 × 24, dynamic padding, and maximized startup.
- On macOS, a transparent title bar and both Option keys acting as Alt.
- 100,000 lines of history, selection copied to the clipboard, and the mouse
  pointer hidden while typing.
- On macOS, JetBrainsMono Nerd Font Mono Light, Regular, Light Italic, and
  Italic from Nerd Fonts v3.5.1, bundled with the original license and README.
- On macOS, Cmd+Shift+[ / ] (including the { / } logical keys) and Ctrl+Tab /
  Ctrl+Shift+Tab select native tabs; Ctrl+Cmd+F toggles maximization.
  Alacritty's existing Cmd+T, Cmd+1–9, and font-size shortcuts still apply.

The Baidu Pinyin Shift/Enter fix remains pinned to the same patched winit.

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
This fork does not include tmux configuration or require tmux to start.

The built-in colors follow the official
[Catppuccin Mocha palette](https://github.com/catppuccin/alacritty/blob/main/catppuccin-mocha.toml),
licensed under MIT; the original notice is in `LICENSE-CATPPUCCIN`.

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

Running only `target/release/alacritty` retains the embedded settings but has
no bundled font resources; it uses an installed matching font or falls back
to Menlo. Use the `.app` bundle for a self-contained macOS installation.
Other platforms retain their native default font families.
`TERM` retains upstream's automatic detection, using `xterm-256color` when
Alacritty's terminfo has not been separately installed on a fresh system.

## Validation

```sh
cargo test --locked -p alacritty --bin alacritty config::
cargo build --locked --release --bin alacritty
```

The configuration tests cover no user file, imports, partial overrides,
reload, CLI overrides, missing-file fallback, and replacement of matching
default key bindings.
