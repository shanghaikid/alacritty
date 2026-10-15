use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::term::{Config, Term, test::TermSize};
use alacritty_terminal::vte::ansi::Processor;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Listener(Arc<Mutex<Vec<Event>>>);
impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        self.0.lock().unwrap().push(event);
    }
}

#[test]
fn protocol_lifecycle_and_screen_independence() {
    let listener = Listener::default();
    let mut term = Term::new(Config::default(), &TermSize::new(80, 24), listener.clone());
    let mut parser: Processor = Processor::new();
    parser.advance(
        &mut term,
        b"\x1b]7501;state=working:app=build\x1b\\\x1b]7501;state=done:id=tests\x07",
    );
    assert_eq!(term.program_status.len(), 2);
    for sequence in [b"\x1b[?1049h".as_slice(), b"\x1b[?1049l", b"\x1b[!p"] {
        parser.advance(&mut term, sequence);
        assert_eq!(term.program_status.len(), 2);
    }
    parser.advance(&mut term, b"\x1b]133;A;ignored=parameter\x07");
    assert_eq!(term.program_status.len(), 1);
    assert_eq!(term.program_status.summary().unwrap().state.name(), "done");
    parser.advance(&mut term, b"\x1b]7501;state=blocked\x07");
    term.exit();
    assert_eq!(term.program_status.len(), 1);
    parser.advance(&mut term, b"\x1bc");
    assert!(term.program_status.is_empty());
}

#[test]
fn query_only_replies_with_constants_and_never_record_text() {
    let listener = Listener::default();
    let mut term = Term::new(Config::default(), &TermSize::new(80, 24), listener.clone());
    let mut parser: Processor = Processor::new();
    parser.advance(
        &mut term,
        b"\x1b]7501;state=done:msg=c2VjcmV0\x07\x1b]7501;?\x1b\\\x1b]7501;?\x07",
    );
    let events = listener.0.lock().unwrap();
    let replies: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::PtyWriteStatic(s) => Some(*s),
            _ => None,
        })
        .collect();
    assert_eq!(
        replies,
        vec![
            alacritty_terminal::program_status::REPLY_ST,
            alacritty_terminal::program_status::REPLY_BEL
        ]
    );
    assert!(!replies.iter().any(|s| s.contains("secret")));
}

#[test]
fn report_storm_coalesces_and_duplicates_do_not_notify_or_redraw_grid() {
    let listener = Listener::default();
    let mut term = Term::new(Config::default(), &TermSize::new(80, 24), listener.clone());
    let mut parser: Processor = Processor::new();
    for i in 0..100_000 {
        parser.advance(
            &mut term,
            if i % 2 == 0 {
                b"\x1b]7501;state=working:progress=40\x07"
            } else {
                b"\x1b]7501;state=working:progress=41\x07"
            },
        );
        term.flush_program_status();
        assert!(!parser.take_non_status_activity());
    }
    assert_eq!(listener.0.lock().unwrap().len(), 1);
    term.acknowledge_program_status();
    listener.0.lock().unwrap().clear();
    for _ in 0..1000 {
        parser.advance(&mut term, b"\x1b]7501;state=working:progress=41\x07");
        term.flush_program_status();
    }
    assert!(listener.0.lock().unwrap().is_empty());
    parser.advance(&mut term, b"after status\r\n\x1b[31mred\x1b[0m");
    assert!(parser.take_non_status_activity());
}
