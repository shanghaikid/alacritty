//! CPU-side rendering regressions and an opt-in benchmark, without a GL context.

use std::hint::black_box;
use std::time::Instant;

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::term::Config;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::vte::ansi::{Handler, Processor};

use super::*;

fn content<'a>(
    term: &'a Term<VoidListener>,
    config: &'a UiConfig,
    colors: &'a List,
    size: &'a SizeInfo,
) -> RenderableContent<'a> {
    let terminal_content = term.renderable_content();
    let cursor_point =
        term::point_to_viewport(terminal_content.display_offset, terminal_content.cursor.point)
            .unwrap();
    RenderableContent {
        terminal_content,
        cursor: RenderableCursor::new_hidden(),
        cursor_shape: CursorShape::Hidden,
        cursor_point,
        search: None,
        hint: None,
        config,
        colors,
        focused_match: None,
        size,
        default_colors: None,
    }
}

#[test]
fn snapshot_shares_extras_and_survives_concurrent_grid_changes() {
    let mut term = Term::new(Config::default(), &TermSize::new(20, 2), VoidListener);
    let mut parser: Processor = Processor::new();
    parser.advance(&mut term, "\x1b]8;id=test;https://example.com\x1b\\e\u{301}".as_bytes());
    let config = UiConfig::default();
    let colors = List::from(&config.colors);
    let size = SizeInfo::new(200., 40., 10., 20., 0., 0., false);
    let snapshot: Vec<_> = content(&term, &config, &colors, &size).collect();
    let extra = snapshot[0].extra.as_ref().unwrap();
    assert!(Arc::ptr_eq(extra, term.grid()[Line(0)][Column(0)].extra.as_ref().unwrap()));
    assert_eq!(extra.zerowidth(), &['\u{301}']);
    assert_eq!(extra.hyperlink().unwrap().uri(), "https://example.com");

    // The renderer releases the lock before drawing. PTY writes must not change
    // the combining marks or hyperlink of the frame it has already collected.
    term.input('\u{302}');
    assert_eq!(extra.zerowidth(), &['\u{301}']);
    assert_eq!(
        term.grid()[Line(0)][Column(0)].zerowidth(),
        Some(['\u{301}', '\u{302}'].as_slice())
    );
    term.goto(0, 0);
    term.input('x');
    assert_eq!(snapshot[0].character, 'e');
    assert_eq!(extra.hyperlink().unwrap().uri(), "https://example.com");
}

#[test]
fn cached_base_colors_do_not_leak_between_styles() {
    let term = Term::new(Config::default(), &TermSize::new(20, 2), VoidListener);
    let size = SizeInfo::new(200., 40., 10., 20., 0., 0., false);
    for brighten_bold in [false, true] {
        let mut config = UiConfig::default();
        config.colors.draw_bold_text_with_bright_colors = brighten_bold;
        let colors = List::from(&config.colors);
        let mut content = content(&term, &config, &colors, &size);
        let inputs = [
            Color::Named(NamedColor::Foreground),
            Color::Named(NamedColor::Background),
            Color::Named(NamedColor::Red),
            Color::Indexed(2),
            Color::Indexed(200),
            Color::Spec(alacritty_terminal::vte::ansi::Rgb { r: 12, g: 34, b: 56 }),
        ];
        for fg in inputs {
            for bg in inputs {
                for flags in [Flags::empty(), Flags::BOLD, Flags::DIM, Flags::DIM_BOLD] {
                    for inverse in [Flags::empty(), Flags::INVERSE] {
                        let flags = flags | inverse;
                        let cell = Cell { fg, bg, flags, ..Cell::default() };
                        let mut expected_fg = RenderableCell::compute_fg_rgb(&content, fg, flags);
                        let mut expected_bg = RenderableCell::compute_bg_rgb(&content, bg);
                        let expected_alpha = if inverse.is_empty() {
                            RenderableCell::compute_bg_alpha(&config, bg)
                        } else {
                            mem::swap(&mut expected_fg, &mut expected_bg);
                            1.
                        };
                        let expected = (expected_fg, expected_bg, expected_alpha);
                        assert_eq!(content.base_colors(&cell), expected);
                        assert_eq!(content.base_colors(&cell), expected);
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "manual release-mode CPU snapshot benchmark"]
fn benchmark_render_snapshot() {
    let config = UiConfig::default();
    let colors = List::from(&config.colors);
    let size = SizeInfo::new(2400., 1400., 10., 20., 0., 0., false);
    for (name, prefix, text) in [
        ("plain", "", "e"),
        ("hyperlink", "\x1b]8;id=test;https://example.com\x1b\\", "e"),
        ("combining", "", "e\u{301}"),
        ("underline-color", "\x1b[58;5;2m", "e"),
        ("ansi-dense", "", "\x1b[31me\x1b[32me"),
        ("ansi-words", "", "\x1b[31mconst\x1b[32mvalue"),
    ] {
        let mut term = Term::new(Config::default(), &TermSize::new(240, 70), VoidListener);
        let mut parser: Processor = Processor::new();
        let row = text.repeat(239) + "\r\n";
        parser.advance(&mut term, (prefix.to_owned() + &row.repeat(70)).as_bytes());
        let mut cells = Vec::new();
        let iterations = 1000;
        for _ in 0..20 {
            cells.extend(content(&term, &config, &colors, &size));
            black_box(&cells);
            cells.clear();
        }
        let start = Instant::now();
        for _ in 0..iterations {
            cells.extend(content(black_box(&term), &config, &colors, &size));
            black_box(&cells);
            cells.clear();
        }
        println!(
            "snapshot {name}: {:.2} us/frame",
            start.elapsed().as_secs_f64() * 1e6 / iterations as f64
        );
    }
}
