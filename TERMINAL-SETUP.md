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
  OSC 7501 status validator and applies `extra/tmux-input-cursor.patch` so a
  single-click selection anchor does not move the input caret. The build manifest
  records the patch checksum. The top tab bar and pane borders use Merino colors.
- Cmd+T / Cmd+W tabs, Cmd+D / Cmd+Shift+D splits, Cmd+H/J/K/L pane navigation,
  Cmd+1–9 tab selection, and the existing mouse and Shift-selection behavior.
  Cmd+Shift+N opens a shell window outside tmux; Ctrl+B remains the tmux prefix.
- Scrollback position and search counts without per-line timestamp formatting.
  This avoids repeated macOS timezone-file reads while scrolling in tmux. Mouse
  and trackpad scroll distances are unchanged.
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

The same upgrade also installs `tmux-scroll.conf` through the managed theme
bootstrap, so the scroll fix does not depend on copying this machine's dotfiles.
After upgrading an existing server, apply it without closing any panes:

```sh
tmux source-file ~/.config/alacritty/tmux.conf
tmux show-options -gw copy-mode-position-format
```

The displayed format should retain `copy_position` and `search_count` but omit
`top_line_time`. Dotfiles setups must keep the optional `theme.conf` include
described above for the fix to load when their server restarts. Older tmux
versions without this position-format option skip it without an error. Merely
replacing the `.app` does not update a separately installed tmux configuration.

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

For a recoverable server replacement, `extra/tmux-upgrade.py` accepts an explicit
operator-checked JSON plan containing the old/new sockets, original server PID
and pane inventory, window layouts, working directories, Codex session IDs, and
watched rollout paths. Saved terminal text and `start-tmux.zsh` belong beside the
plan in a private backup directory. The migration fixture in
`extra/test-tmux-upgrade.py` provides a complete plan example. Keep live plans,
conversation files, and terminal captures outside version control.

Run `stage PLAN.json` to create gated replacement panes, then `arm PLAN.json` to
schedule the cutover. The macOS job uses `RunAtLoad=true`, `KeepAlive=false`, and a
plist in the backup directory, so it runs once without creating a login item.
The helper waits for the watched turns to finish; skipping a watched conversation
requires an explicit `wait_for` subset and `interruption_authorized=true`.
Completed or failed cutovers retain their terminal status when invoked again.
`verify PLAN.json` checks the restored processes and thread histories across
shared Codex backends. Codex conversations resume by ID; shells restart in their
saved directories with captured terminal text available in scrollback.

`TEST_TMUX=/path/to/tmux python3 extra/test-tmux-upgrade.py` exercises client
handoff, layouts, shared-backend readiness, and repeated invocation using fake
Codex processes. Add `--launchd` on macOS to test the actual one-shot job as well.

To restore previous settings, quit Alacritty and move the desired backed-up app
and configuration files back to their original paths. Remove the marked
Alacritty block from `.zshrc` if there was no previous file. The two app-specific
preferences changed by installation are `AppleFontSmoothing` and Hide-menu
entries in `NSUserKeyEquivalents` under `org.alacritty`; these can be changed
with macOS `defaults`. The previous preference domain is saved as
`org.alacritty-before.plist` in the configuration backup (empty if no previous
domain existed). Other applications' rendering preferences are untouched.

## Codex notifications on macOS

The title-bar bell switches Codex notifications on or off for the application.
It uses a fixed-size native symbol, synchronizes across windows, and remembers
the setting in `org.alacritty` → `CodexNotificationsEnabled` (default: on).
Turning it off withdraws existing Codex notifications; turning it back on does
not replay events that happened while muted. Ordinary shell-command completion
notifications and OSC status badges are independent of this switch.

The installed `codex-program-status` hook starts one local observer per live
terminal Codex session. `codex_notifications.py` handles completion, structured
`request_user_input` / `request_user_input_async` questions, and manual approval
requests. Automatic approval review is silent. Plain prose is not classified as
a question by punctuation. A pending asynchronous question remains pending
after the tool returns `accepted`, and is cleared by its matching answer.

Completion waits for the turn to end and a short settling interval. Active
goals suppress intermediate completions; a completed goal notifies after its
final reply. Blocked goals and budget/usage stops have distinct messages.
`thread/goal/get` is the primary source of goal state; a read-only, schema-checked
adapter for Codex 0.166's `goals_1.sqlite` supports standalone CLI installations
without the shared daemon. Unknown schemas/API failures never imply success.
The incremental transcript adapter is version-specific and must be checked when
upgrading Codex. It does not load transcripts into the model or copy their text
into notifications/state files.

The observer resolves the live tmux client rather than trusting a stale socket
inherited from an old tmux server. Notifications are suppressed only when the
corresponding window and pane are being viewed. Clicking one focuses that window
and pane. Viewing the session, answering the question, starting new work, or
ending the session removes obsolete notifications. Each session has one stable
notification identifier; a dismissed event does not repeatedly alert.

macOS must allow Alacritty notifications in System Settings → Notifications.
The existing Codex `notify` command is not replaced. Existing trusted hook
definitions do not change during this upgrade; a fresh installation still needs
the standard Codex `/hooks` trust step. The runtime diagnostics are under
`${XDG_STATE_HOME:-~/.local/state}/codex-notifications/<session>/status.json`;
these contain lifecycle identifiers/counts, not prompts or tool output.

After upgrading the app and installed helper, open a window with the new binary.
Existing tmux sessions remain intact. An old running Alacritty process does not
gain the new native notification or title-bar code until it is restarted.

## Validation

```sh
PYTHONDONTWRITEBYTECODE=1 python3 extra/test-codex-notifications.py
PYTHONDONTWRITEBYTECODE=1 python3 extra/codex-notification-acceptance.py dist/Alacritty.app
```

Notification acceptance uses a disposable app process, tmux server, observer
processes, and synthetic lifecycle records. It exercises the real macOS
notification center, real NSButton target/action, and installed notification
delegate; it does not synthesize physical input or alter existing terminal
sessions. The bell preference is restored afterward. Rust compilation should
use an isolated temporary target, with cleanup on failure and success.

```sh
TEST_TMUX="$PWD/dist/Alacritty.app/Contents/MacOS/tmux" \
  python3 extra/test-terminal-install.py
```

Tests use a temporary home and a separate tmux socket. They check TOML parsing,
repeat installation, existing symlinks and hooks, zsh prompt isolation, actual
tmux colors, selection bindings, OSC status reporting, and scroll settings across
fresh installation, live upgrade, and server restart. To stage the complete
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
