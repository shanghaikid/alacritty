# macOS IME fix

This fork is based on Alacritty 0.17.0 and uses a patched winit 0.30.13.
The default branch is `macos-ime-fix`.

With Baidu Pinyin in Chinese mode, typing an English word and pressing Shift
commits the word. In the original version, the following Enter key is dropped;
the second Enter works. This also occurs without tmux.

The winit patch clears a `Committed` state left by a previous event before
interpreting a new key. Text committed while interpreting the current key still
suppresses that key, so confirming composition with Enter does not also execute
a terminal command.

`Cargo.toml` pins the patched winit commit in
[shanghaikid/winit](https://github.com/shanghaikid/winit/tree/c73f0893b0f3d4e2bb6231d94e742434f36d330c).
Cargo downloads that dependency automatically.

## Build on macOS

Install Rust and the Xcode Command Line Tools, then:

```sh
git clone https://github.com/shanghaikid/alacritty.git
cd alacritty
cargo build --locked --release --bin alacritty
./target/release/alacritty
```

The binary includes [our built-in defaults](LOCAL-DEFAULTS.md), with normal
Alacritty configuration files available for overrides. To create an app bundle
with the included fonts, install `scdoc` and run `make app`.

## Validation

- Built on macOS using the existing Alacritty dependency versions.
- With a raw input receiver outside tmux, Shift-committed text followed by Enter
  produces the first Enter key press and a carriage return.
- When Enter commits composition text, the receiver gets the committed text
  without an extra carriage return.

The original package's `dpi` dependency is kept from crates.io rather than the
older workspace copy in the winit Git repository.

## Updates

```sh
git pull --ff-only
cargo build --locked --release --bin alacritty
```

The `upstream` remote in the original local checkout points to the official
repository. Update this fork deliberately against newer upstream releases and
repeat the IME checks before replacing an installed app.
