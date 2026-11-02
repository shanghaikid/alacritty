# Install this terminal on another Mac

For **macOS on Apple Silicon**, run these commands from a native arm64 terminal.
Install Xcode Command Line Tools (`xcode-select --install`), Homebrew, and a Rust
toolchain supporting the repository's `rust-version` first.

```sh
brew install python rust scdoc autoconf automake pkg-config libevent ncurses utf8proc jemalloc
git clone --branch macos-ime-fix https://github.com/shanghaikid/alacritty.git
cd alacritty
make install-terminal
open /Applications/Alacritty.app
```

If Rust is already managed by rustup, omit `rust` from the Homebrew command.
Do not run the installer with `sudo`: it installs configuration for the current
user. If `/Applications` is not writable, use the user Applications directory:

```sh
make terminal
python3 dist/Alacritty.app/Contents/Resources/terminal-profile/install.py \
  --app-dir "$HOME/Applications"
open "$HOME/Applications/Alacritty.app"
```

No dotfiles checkout, Rex installation, copied home directory, or hardcoded
username is needed. This workflow is separate from upstream's `make app`, which
builds only the terminal emulator.

## What is included

- Merino Light terminal colors, Menlo 12, natural line height, a blinking block
  cursor, Light decorations, and app-specific font smoothing disabled.
- A native **◐** button at the right of the macOS title bar switches between
  Merino Light and Merino Dark. It updates the palette, native appearance, and
  registered tmux servers together, and remembers the selection across restarts
  and upgrades. The button occupies existing title-bar space, without adding a row.
- A bundled tmux built from revision
  `b60f804c97adee1aaa7bd9f005af90bdfc0b949d` of
  [shanghaikid/tmux](https://github.com/shanghaikid/tmux). It links this checkout's
  OSC 7501 status validator. The top tab bar and pane borders use Merino colors.
- Cmd+T / Cmd+W tabs, Cmd+D / Cmd+Shift+D splits, Cmd+H/J/K/L pane navigation,
  Cmd+1–9 tab selection, and the existing mouse and Shift-selection behavior.
  Cmd+Shift+N opens a shell window outside tmux; Ctrl+B remains the tmux prefix.
- A zsh prompt with a blue path, amber Git branch, green/red command indicator,
  and subdued time. A guarded block is appended to the user's `.zshrc`; the
  profile applies only when `ALACRITTY_TERMINAL_PROFILE=merino`, set by Alacritty.
  Existing shell setup and other terminals' prompts remain available.
- Codex OSC 7501 lifecycle reporting, merged into existing hooks. Install Codex
  CLI separately and, once on the new Mac, open `/hooks` and trust the
  `codex-program-status` entries. The installer does not copy credentials, chat
  history, or hook trust. Codex versions without these hooks will not emit badges.

The tmux server is named `alacritty-merino`. Reopening Alacritty reattaches to
the same session without reloading key tables or creating extra tabs. Other
tmux servers, including this machine's older `default` server, are left running.
Inside the managed panes, `tmux` resolves to the bundled binary.

## Existing dotfiles-based machines

Keep the existing dotfiles and tmux launcher. To add the native button without
replacing `.zshrc`, `.tmux.conf`, their symlinks, or personal Alacritty settings:

```sh
git pull --ff-only
make terminal
python3 extra/install-merino-toggle.py
```

Then quit Alacritty, install `dist/Alacritty.app` over the previous app, and reopen
it. The upgrade saves the previous main config as `alacritty-before-theme.toml`
and imports it before the active theme. If it was a dotfiles symlink, the saved
import remains linked. Existing non-theme settings keep working.
The installer prints the backup directory. It does not edit shell startup files.

The updated dotfiles `.tmux.conf` conditionally loads
`~/.config/alacritty/theme.conf`, so reloading shared settings keeps the selected
theme. Its installer also preserves the managed Alacritty config. For an already
running server, source `~/.config/alacritty/tmux.conf` once; this registers the
server for native button updates. The new-machine installer does this at startup.

The current dotfiles prompt uses ANSI `blue`, `yellow`, `green`, `red`, and color
8, so both existing and new zsh prompts follow the selected palette automatically.
Programs that hardcode RGB colors or cache terminal colors, including existing
Codex instances, may need their own theme setting or a restart/resume. Switching
Merino does not restart applications, interrupt commands, or change Codex trust.
The official Codex CLI is kept unchanged. In the tested Codex 0.166.0, input-box
colors are cached at startup; after switching, use `/quit`, then `codex resume`
to choose the original conversation. `/theme` changes syntax highlighting and
does not refresh that background cache.

The theme helper is also usable from a shell:

```sh
# Existing-machine overlay; use terminal-profile instead on fresh installations.
python3 ~/.config/alacritty/terminal-theme/theme.py toggle
```

## Installation and upgrades

`make terminal` leaves a signed local bundle at `dist/Alacritty.app`. Rust and
tmux build intermediates live in a dedicated temporary directory and are removed
on success or failure; only the completed bundle is retained. The build uses
Cargo.lock and a fixed tmux revision. Homebrew runtime libraries are required;
build on the destination Mac rather than treating this as a standalone binary
distribution for machines without those libraries. The bundle's minimum macOS
version is set to the build machine's version to match its Homebrew dependencies.

`make install-terminal` builds and runs the installer. It copies the bundle and
the managed profile into `~/.config/alacritty/terminal-profile`, writes the main
Alacritty config, and installs the status reporter in `~/.local/bin`.
`XDG_CONFIG_HOME`, `ZDOTDIR`, and `CODEX_HOME` are respected when set during
installation. Run installation with the same environment used by your shell.

Existing files and symlinks that are replaced are moved into timestamped
`~/.alacritty-backups/` directories, whose paths are printed. Symlink targets in
external dotfiles repositories are not edited. Existing hooks are preserved;
repeating installation does not duplicate hooks or the zsh source block.
The main Alacritty config is intentionally replaced by this complete profile;
reapply any desired personal overrides from its backup afterward.

On upgrade, quit and reopen Alacritty. Reload tmux settings with Ctrl+B, R in
the managed session. To switch an already-running tmux server to a newly built
binary, finish its work and exit its panes, then reopen Alacritty. The installer
does not terminate sessions. Codex instances started under a dark theme must
also be restarted/resumed because they cache terminal colors at startup.

To restore previous settings, quit Alacritty and move the desired backed-up app
and configuration files back to their original paths. Remove the marked
Alacritty block from `.zshrc` if there was no previous file. The two app-specific
preferences changed by installation are `AppleFontSmoothing` and Hide-menu
entries in `NSUserKeyEquivalents` under `org.alacritty`; these can be changed
with macOS `defaults`. The previous preference domain is saved as
`org.alacritty-before.plist` in the configuration backup (empty if no previous
domain existed). Other applications' rendering preferences are untouched.

## Validation

```sh
TEST_TMUX="$PWD/dist/Alacritty.app/Contents/MacOS/tmux" \
  python3 extra/test-terminal-install.py
```

Tests use a temporary home and a separate tmux socket. They check TOML parsing,
repeat installation, existing symlinks and hooks, zsh prompt isolation, actual
tmux colors, selection bindings, and OSC status reporting. To stage the complete
installer without changing your real configuration or macOS preferences:

```sh
staging=$(mktemp -d)
python3 dist/Alacritty.app/Contents/Resources/terminal-profile/install.py \
  --home "$staging/home" --app-dir "$staging/Applications" --skip-preferences
```

`python3 extra/theme-button-acceptance.py dist/Alacritty.app` additionally runs a
disposable macOS app with a temporary native probe. It clicks the real NSButton,
checks runtime Alacritty and tmux colors, verifies fixed content dimensions,
resizes and reloads the control, and reopens the app to verify persistence.
It does not inject code into an installed or already running application.

The profile assets originally maintained in `shanghaikid/dotfiles` are now kept
here so the installer has a single source checkout. Do not copy personal shell
startup files or credentials into this package.
