# iTerm2 appearance and rendering parity

Status: implemented and installed, 2026-10-23. The user accepted the font,
terminal theme and titlebar spacing. The subsequent request to include the top
tmux tabs is also applied and automatically verified.

## Appearance and font rendering

The local iTerm2 3.6.6 Default profile uses SF Mono 12, horizontal spacing 1.0,
vertical spacing 0.92, an opaque background and a blinking beam cursor.
`extra/iterm2-profile-export.m` converts its untagged calibrated RGB colors to
sRGB with macOS ColorSync. `extra/iterm2-profile.toml` contains the migrated theme
and is installed in `~/.config/alacritty/`, imported after the shared dotfiles.
Fonts are already installed locally; no Apple fonts are redistributed.

- Background `#252525`, foreground `#e3f1e4`, selection `#606060`, and the full
  ANSI palette come from the exported profile.
- `font.line_height = 14.0` matches the reference font metrics and line spacing.
  The setting scales with font size and DPI and also sizes box drawing.
  The tested Retina display reports 14×28 pixel cells.
- Explicit `colors.indexed_colors` now replaces the embedded palette, allowing
  an empty array to restore standard indices 16/17 instead of Catppuccin values.
- App-specific `org.alacritty AppleFontSmoothing = 0` gives the thin strokes the
  user confirmed are close to iTerm2. A new process is required for this setting.
- Padding is 5 points horizontally and 24 vertically. A native probe measured
  the traffic-light button bottom at 22 points: terminal text clears it by 2.

## Top tabs and tmux persistence

`extra/iterm2-tmux.conf` applies the same colors to tabs, status bar, clock,
borders, messages and copy-mode selection. The selected tab uses the profile's
selection background and white text; inactive tabs use its dark background.
These are derived terminal-palette colors, not a pixel capture of native iTerm2
window chrome.

The active Alacritty session is on tmux's `default` socket. An earlier startup
configuration targeted `osc7501`, but imported shell argument arrays appended,
so that startup script did not execute. The installed main config now uses a
string `terminal.shell` pointing to the executable startup script; this replaces
the imported shell table and prevents argument accumulation.

`extra/start-iterm2-tmux.zsh` is installed as
`~/.config/alacritty/start-tmux.zsh`. It preserves the existing `alacritty` session
on the default socket, loads the local tmux config, and attaches. The local config
loads the theme after shared settings, including when reloaded with prefix+r.
The default server contained only the Alacritty session at installation. Its
palette overrides are server defaults and apply to future windows as well.
Both existing windows were checked for effective styles; pane IDs and processes
were unchanged. Other tmux sockets and the shared dotfiles were not modified.

## Chinese and complex Emoji

macOS defaults enable `terminal.grapheme_clusters`. Incremental Unicode grapheme
assembly handles fragmented PTY input, widths, wrapping and grid storage.
CoreText shapes and caches complete clusters, including ZWJ sequences, flags,
skin tones, variation selectors and keycaps. Ordinary glyphs keep the existing
cache. Selection, copy, resize, regex search and cache invalidation are covered
by regression tests. Common CJK input has a segmentation fast path.

Cursor-position probes now agree with iTerm2 for every measured case:

| Text | Width in cells |
| --- | ---: |
| 中文 | 4 |
| e + combining acute | 1 |
| 😀, 🚀, 👩‍💻, 🇨🇳, 👍🏽, ❤️ | 2 each |

Own-process framebuffer inspection confirmed complete color Emoji, Chinese,
accents, regular/bold/italic text and TUI borders, directly and through tmux.
The final installed framebuffer matched the staged rendering byte for byte.

## Verification and performance

- Workspace tests after review fixes: 316 passed, 3 ignored, no failures.
- Subsequent targeted checks: all 7 grapheme tests after CJK optimization and all
  33 app config tests after enabling the macOS default passed.
- Review added two regressions and fixed both: widening an Emoji at the right
  margin over an existing wide character no longer erases the new cluster;
  its spacer preserves display attributes instead of losing inverse/underline.
- Clippy for both app and terminal, all targets with warnings denied: passed.
- Final `make app`, strict app signature verification, and installed/source
  executable SHA-256 comparison: passed.
- Installed config loads without warnings/errors; cell size 14×28 and padding
  10×48 pixels. Actual normal startup was separately checked after the shell fix.
- Stable rustfmt was applied to touched files. Whole-tree formatting has existing
  differences and nightly-only settings; unrelated files were not reformatted.

`extra/terminal-io-bench.py` measures PTY processing and cursor-report round trips.
With equal 1000-line history, the initial three-pair iTerm2 baseline was 45.03
MiB/s ASCII, 6.29 MiB/s ANSI+CJK and 24.93 ms cursor-report median. Three later
Alacritty runs after grapheme/CJK optimization measured medians of 48.71 MiB/s,
47.33 MiB/s and 0.202 ms. These later runs were not interleaved with iTerm2.

The final installed app with its actual 100000-line history measured 41.02 MiB/s
ASCII, 40.83 MiB/s ANSI+CJK and 0.221 ms cursor-report median (p95 1.612 ms).
The final isolated tmux run measured 22.23 MiB/s, 10.92 MiB/s and 0.241 ms.
These are workload-specific measurements, not a general speedup guarantee.

`extra/frame-pacing-acceptance.py` measured 240 scrolling frames at 59.96 FPS,
with interval median 16.49 ms and p95 19.27 ms. For 100 locally injected native
key events, event-to-GPU-marker latency was median 2.825 ms and p95 3.518 ms.
This measures software response through GPU output, not physical keyboard or
display scanout latency. The pacing run used the same enabled cluster rendering
path before the CJK fast-path optimization and default-config switch.

## Limits

No iTerm2 screenshot was taken: the user chose automatic testing without screen
recording permission. Appearance was confirmed by the user, native profile
extraction and our own isolated app framebuffer probes. This is not a claim of
pixel-identical iTerm2 rendering in every circumstance. App-specific thin strokes
also apply on non-Retina displays; automatic per-display matching is unverified.
Font zoom scales line height proportionally instead of repeating iTerm2's rounding
at each size. General faint/truecolor blending still follows Alacritty behavior.

## Installation, artifacts and rollback

Installed app: `~/Applications/Alacritty.app`. The original appearance acceptance
used base `cc57d128` plus working-tree changes. The review build includes the
wide-character overwrite and spacer-attribute fixes described above.

- Before appearance migration: app backup
  `~/Applications/Alacritty.backup-iterm-20261023-155713.app` and main config
  `~/.config/alacritty/alacritty.toml.bak-iterm-20261023-155713`.
- Before final Emoji build: app backup
  `~/Applications/Alacritty.backup-before-emoji-20261023-165127.app`; main config,
  profile and tmux config have `.bak-final-20261023-165127` copies.
- Before tab/startup correction: main config, startup script, tmux config and
  tmux theme have `.bak-tabs-20261023-170550` copies in `~/.config/alacritty/`.
- Logs, raw JSON, own-process framebuffer captures and installation manifests:
  `/private/tmp/alacritty-iterm-validation/` (temporary artifacts).
- Native exported profile: `/private/tmp/iterm2-appearance.json`.
- Local iTerm2 reference: `/private/tmp/alacritty-iterm-reference/`, tag v3.6.6,
  commit `6c97d87e6cb074de48b20eb244bd5d513a6c9308`.

Rollback requires selecting the matching app/config backup stage. To undo the
original thin-stroke preference, remove only `AppleFontSmoothing` from the
`org.alacritty` defaults domain; it was previously absent. Reload the previous
tmux config to revert live tab colors. Existing user sessions were not closed.

Reference behavior was checked in the local iTerm2 source files
`NSDictionary+iTerm.m`, `FontSizeEstimator.m`, `PTYTextView.m`,
`iTermProfilePreferences.m` and `Metal/Support/iTermCharacterSource.m`.
No iTerm2 implementation code was copied into the application.
