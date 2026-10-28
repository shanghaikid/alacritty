//! Compare cold search, viewport reads and reflow with identical history data.
//! Run in release mode. This measures terminal operations, not GPU frames.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::hint::black_box;
use std::time::Instant;

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{CompactRow, Dimensions, Grid, Row, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point, Side};
use alacritty_terminal::term::cell::Cell;
use alacritty_terminal::term::search::RegexSearch;
use alacritty_terminal::term::{Config, Term, test::TermSize};
use alacritty_terminal::vte::ansi::Processor;

fn digest(grid: &Grid<Cell>) -> u64 {
    let mut hash = DefaultHasher::new();
    grid.history_size().hash(&mut hash);
    for line in grid.topmost_line().0..=grid.bottommost_line().0 {
        for cell in &grid[Line(line)] {
            cell.c.hash(&mut hash);
            cell.flags.bits().hash(&mut hash);
            cell.zerowidth().hash(&mut hash);
        }
    }
    hash.finish()
}

fn main() {
    eprintln!(
        "Storage layout: Cell={} Row<Cell>={} CompactRow<Cell>={}",
        std::mem::size_of::<Cell>(),
        std::mem::size_of::<Row<Cell>>(),
        std::mem::size_of::<CompactRow<Cell>>(),
    );
    let history: usize = std::env::args().nth(1).map_or(100_000, |n| n.parse().unwrap());
    assert!(history >= 1000);
    for (corpus, content) in [
        ("ascii", "0123456789 ascii output ".repeat(4)),
        ("cjk", "中文终端测试".repeat(8)),
        ("graphemes", "中文 e\u{301} 👩‍💻 ".repeat(7)),
    ] {
        let config = Config { scrolling_history: history, ..Config::default() };
        let mut term = Term::new(config.clone(), &TermSize::new(120, 40), VoidListener);
        let mut parser: Processor = Processor::new();
        for n in 0..history + 40 {
            parser.advance(&mut term, format!("LOG-{n:07} {content}\r\n").as_bytes());
        }
        let original = term.grid().clone();
        let mut expected = None;
        for trial in 0..3 {
            for slot in 0..3 {
                let storage = ["dense", "live", "compressed"][(slot + trial) % 3];
                // Prepare each operation independently. Cloning a compressed
                // grid shares block expansion caches, so cloning it after a
                // search would accidentally benchmark warm pages and reflow.
                let prepare = || {
                    let mut grid = original.clone();
                    match storage {
                        "dense" => {
                            for line in grid.topmost_line().0..=grid.bottommost_line().0 {
                                let _ = &mut grid[Line(line)][..];
                            }
                        },
                        "compressed" => {
                            let mut cursor = 0;
                            while grid.compact_history(&mut cursor) {}
                        },
                        _ => (),
                    }
                    grid
                };
                *term.grid_mut() = prepare();
                let mut regex = RegexSearch::new(&format!("LOG-{:07}", history - 50)).unwrap();
                let start = Instant::now();
                let found = term.search_next(
                    &mut regex,
                    Point::new(term.topmost_line(), Column(0)),
                    Direction::Right,
                    Side::Left,
                    None,
                );
                let search_ms = start.elapsed().as_secs_f64() * 1000.;
                assert!(found.is_some());

                *term.grid_mut() = prepare();
                term.grid_mut().scroll_display(Scroll::Top);
                let mut pages = Vec::new();
                for _ in 0..(history / 40).min(500) {
                    let start = Instant::now();
                    let offset = term.grid().display_offset() as i32;
                    let mut chars = 0u64;
                    for line in -offset..40 - offset {
                        for cell in &term.grid()[Line(line)] {
                            chars = chars.wrapping_add(cell.c as u64);
                        }
                    }
                    black_box(chars);
                    term.grid_mut().scroll_display(Scroll::Delta(-40));
                    pages.push(start.elapsed().as_secs_f64() * 1000.);
                }
                pages.sort_by(f64::total_cmp);

                let mut grid = prepare();
                let start = Instant::now();
                for columns in [81, 160, 120] {
                    grid.resize(true, 40, columns);
                }
                let reflow_ms = start.elapsed().as_secs_f64() * 1000.;
                let result = (found, digest(&grid));
                if let Some(expected) = &expected {
                    assert_eq!(&result, expected, "{corpus}/{storage}");
                } else {
                    expected = Some(result);
                }
                println!(
                    "{}",
                    serde_json::json!({
                        "corpus": corpus, "history": history, "storage": storage,
                        "trial": trial, "search_ms": search_ms, "reflow_ms": reflow_ms,
                        "page_p50_ms": pages[pages.len()/2],
                        "page_p95_ms": pages[pages.len()*95/100],
                    })
                );
            }
        }
    }
}
