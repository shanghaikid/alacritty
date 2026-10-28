#![cfg(feature = "ansi")]
use vte::ansi::{Handler, Processor};

#[derive(Default)]
struct Shell {
    markers: Vec<(u8, Option<u32>)>,
    prompts: usize,
}
impl Handler for Shell {
    fn shell_prompt(&mut self) {
        self.prompts += 1;
    }
    fn shell_marker(&mut self, marker: u8, status: Option<u32>) {
        self.markers.push((marker, status));
    }
}

#[test]
fn semantic_markers_accept_bel_st_options_and_fragmented_input() {
    let bytes =
        b"\x1b]133;A;cl=line\x07\x1b]133;B\x1b\\\x1b]133;C\x07\x1b]133;D;127\x1b\\\x1b]133;D\x07";
    for size in 1..=bytes.len() {
        let mut parser: Processor = Processor::new();
        let mut shell = Shell::default();
        for chunk in bytes.chunks(size) {
            parser.advance(&mut shell, chunk);
        }
        assert_eq!(shell.prompts, 1);
        assert_eq!(
            shell.markers,
            vec![(b'A', None), (b'B', None), (b'C', None), (b'D', Some(127)), (b'D', None)]
        );
    }
}

#[test]
fn malformed_markers_and_exit_status_are_ignored() {
    let mut parser: Processor = Processor::new();
    let mut shell = Shell::default();
    for value in ["", ";A", ";D;-1", ";D;+1", ";D;99999999999999999999", ";D;abc", ";UNKNOWN"] {
        let sequence = if value == ";A" {
            "\x1b]1337;A\x07".to_owned()
        } else {
            format!("\x1b]133{value}\x07")
        };
        parser.advance(&mut shell, sequence.as_bytes());
    }
    assert!(shell.markers.is_empty());
    assert_eq!(shell.prompts, 0);
}
