use alacritty_terminal::event::VoidListener;
use alacritty_terminal::term::{Config, Term, test::TermSize};
use alacritty_terminal::vte::ansi::Processor;
use std::hint::black_box;
use std::time::Instant;
fn main() {
    for (name, row) in [
        ("ascii", b"README text 0123456789 abcdefghijklmnopqrstuvwxyz\r\n".as_slice()),
        ("ansi", b"\x1b[32mREADME text\x1b[0m 0123456789\r\n"),
    ] {
        let bytes = row.repeat(2048);
        let mut parser: Processor = Processor::new();
        let mut term = Term::new(Config::default(), &TermSize::new(120, 40), VoidListener);
        for _ in 0..10 {
            parser.advance(&mut term, &bytes);
        }
        let start = Instant::now();
        for _ in 0..200 {
            parser.advance(black_box(&mut term), black_box(&bytes));
        }
        println!(
            "{name} {:.2} MB/s",
            bytes.len() as f64 * 200.0 / start.elapsed().as_secs_f64() / 1e6
        );
    }
}
