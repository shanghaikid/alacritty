use super::*;

#[derive(Default)]
struct Sink {
    reports: Vec<(Vec<Vec<u8>>, bool)>,
}
impl Perform for Sink {
    fn osc_dispatch(&mut self, params: &[&[u8]], bel: bool) {
        self.reports.push((params.iter().map(|p| p.to_vec()).collect(), bel));
    }
}

#[test]
fn program_status_every_chunk_boundary_and_controls() {
    let bytes = b"\x1b]7501;state=working:bad=a;b:msg=eA==\x1b\\";
    for split in 0..=bytes.len() {
        let mut parser = Parser::new();
        let mut sink = Sink::default();
        parser.advance(&mut sink, &bytes[..split]);
        if split < bytes.len() {
            assert!(sink.reports.is_empty());
        }
        parser.advance(&mut sink, &bytes[split..]);
        assert_eq!(
            sink.reports,
            vec![(vec![b"7501".to_vec(), b"state=working:bad=a;b:msg=eA==".to_vec()], false)]
        );
    }
    let mut parser = Parser::new();
    let mut sink = Sink::default();
    parser.advance(&mut sink, b"\x1b]7501;state=working:x=a\x00b\x7fc\x07");
    assert_eq!(sink.reports[0].0[1], b"state=working:x=a\x00b\x7fc");
}

#[test]
fn program_status_wire_limits_abort_and_recover() {
    for (end, limit) in [(b"\x07".as_slice(), 4088), (b"\x1b\\".as_slice(), 4087)] {
        for extra in [0, 1] {
            let mut parser = Parser::new();
            let mut sink = Sink::default();
            parser.advance(&mut sink, b"\x1b]7501;");
            parser.advance(&mut sink, &vec![b'x'; limit + extra]);
            parser.advance(&mut sink, end);
            assert_eq!(sink.reports.len(), usize::from(extra == 0));
            parser.advance(&mut sink, b"\x1b]7501;?\x07");
            assert_eq!(sink.reports.last().unwrap().0[1], b"?");
        }
    }
    for end in [b"\x18".as_slice(), b"\x1a", b"\x1bX"] {
        let mut parser = Parser::new();
        let mut sink = Sink::default();
        parser.advance(&mut sink, b"\x1b]7501;state=done");
        parser.advance(&mut sink, end);
        assert!(sink.reports.is_empty());
    }
}

#[test]
fn program_status_unterminated_flood_is_bounded_other_osc_is_unchanged() {
    let mut parser = Parser::new();
    let mut sink = Sink::default();
    parser.advance(&mut sink, b"\x1b]7501;");
    for _ in 0..1024 {
        parser.advance(&mut sink, &[b'x'; 4096]);
    }
    assert!(parser.osc_raw.len() <= 4092);
    assert!(parser.osc_raw.capacity() <= 4096);
    parser.advance(&mut sink, b"\x1b\\");
    assert!(sink.reports.is_empty());
    parser.advance(&mut sink, b"\x1b]52;c;");
    parser.advance(&mut sink, &[b'x'; 8192]);
    parser.advance(&mut sink, b"\x07");
    assert_eq!(sink.reports[0].0[2].len(), 8192);
}

#[test]
fn program_status_activity_separate_from_grid_with_fragmented_input() {
    let mut parser = Parser::new();
    let mut sink = Sink::default();
    for byte in b"\x1b]7501;state=working\x1b\\" {
        parser.advance(&mut sink, &[*byte]);
        assert!(!parser.take_non_status_activity());
    }
    parser.advance(&mut sink, b"\x1b]7501;?\x07normal text\x1b[31m");
    assert!(parser.take_non_status_activity());
    assert!(!parser.take_non_status_activity());
    parser.advance(&mut sink, b"\x1b]2;title\x07");
    assert!(parser.take_non_status_activity());
}

#[test]
fn fragmented_ordinary_sequences_keep_their_final_wakeup() {
    for (head, tail) in [(b"\xe4\xb8".as_slice(), b"\xad".as_slice()),
                         (b"\x1b[", b"2J"), (b"\x1b#", b"8")] {
        let mut parser = Parser::new();
        let mut sink = Sink::default();
        parser.advance(&mut sink, head);
        parser.take_non_status_activity();
        parser.advance(&mut sink, tail);
        assert!(parser.take_non_status_activity());
    }
}
