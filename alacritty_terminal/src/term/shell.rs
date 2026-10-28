//! Bounded OSC 133 command history. Coordinates belong to the primary grid.
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use super::{Cell, Term, TermMode};
use crate::event::{Event, EventListener};
use crate::grid::{Dimensions, Grid};
use crate::index::{Column, Line, Point};

const MAX_COMMANDS: usize = 4096;

#[derive(Default)]
struct Command {
    prompt: Option<Point>,
    output: Option<Point>,
    end: Option<Point>,
    complete: bool,
}

#[derive(Default)]
pub(super) struct ShellState {
    commands: VecDeque<Command>,
    running: Option<Instant>,
    navigation: Option<usize>,
    pending_scroll: i32,
    pending_top: Line,
}

impl ShellState {
    fn flush_scroll(&mut self) {
        let delta = std::mem::take(&mut self.pending_scroll);
        if delta == 0 {
            return;
        }
        for command in &mut self.commands {
            for point in [&mut command.prompt, &mut command.output, &mut command.end] {
                *point = point.and_then(|mut p| {
                    p.line.0 = p.line.0.saturating_add(delta);
                    (p.line >= self.pending_top).then_some(p)
                });
            }
        }
    }

    pub(super) fn queue_scroll(&mut self, lines: usize, top: Line) {
        self.pending_scroll = self.pending_scroll.saturating_sub(lines as i32);
        self.pending_top = top;
    }

    fn effective(&self, mut point: Point) -> Option<Point> {
        point.line.0 = point.line.0.saturating_add(self.pending_scroll);
        (self.pending_scroll == 0 || point.line >= self.pending_top).then_some(point)
    }

    pub(super) fn map_points(&mut self, mut map: impl FnMut(Point) -> Option<Point>) {
        self.flush_scroll();
        for command in &mut self.commands {
            for point in [&mut command.prompt, &mut command.output, &mut command.end] {
                *point = point.and_then(&mut map);
            }
        }
    }

    pub(super) fn scroll(&mut self, start: Line, end: Line, delta: i32, top: Line) {
        self.map_points(|mut point| {
            if point.line >= start && point.line < end {
                point.line += delta;
                if point.line < start.max(top) || point.line >= end {
                    return None;
                }
            }
            (point.line >= top).then_some(point)
        });
    }

    /// Temporarily attach sparse anchors to cells so the grid's own reflow algorithm maps them.
    /// No per-cell metadata is retained during normal rendering or history compression.
    pub(super) fn resize(&mut self, grid: &mut Grid<Cell>, lines: usize, columns: usize) {
        if columns == grid.columns() {
            let old_line = grid.cursor.point.line;
            grid.resize(true, lines, columns);
            let delta = grid.cursor.point.line.0 - old_line.0;
            self.map_points(|mut p| {
                p.line += delta;
                (p.line >= grid.topmost_line() && p.line <= grid.bottommost_line()).then_some(p)
            });
            return;
        }
        let mut anchors = BTreeMap::new();
        self.map_points(|point| {
            if point.line < grid.topmost_line() || point.line > grid.bottommost_line() {
                return None;
            }
            let index = anchors.len();
            anchors.entry(point).or_insert(index);
            Some(point)
        });
        if anchors.is_empty() {
            grid.resize(true, lines, columns);
            return;
        }
        // Boundaries after a cell must travel with that cell. Marking the blank
        // cell after an exclusive end would introduce content during reflow and
        // could change a pending-wrap cursor into a cursor on a new line.
        let anchor_cell =
            |point: Point| Point::new(point.line, Column(point.column.0.saturating_sub(1)));
        let mut cells = BTreeMap::new();
        for point in anchors.keys() {
            let cell = anchor_cell(*point);
            let id = cells.len();
            cells.entry(cell).or_insert(id);
        }
        for (&point, &id) in &cells {
            grid[point].set_reflow_anchor(Some(id));
        }
        grid.resize(true, lines, columns);
        let mut mapped = BTreeMap::new();
        // Reflow already visits the history. Only rows carrying an anchor are mutated here.
        for line in grid.topmost_line().0..=grid.bottommost_line().0 {
            let line = Line(line);
            let found: Vec<_> = grid[line][..]
                .iter()
                .enumerate()
                .filter_map(|(col, cell)| cell.reflow_anchor().map(|id| (Column(col), id)))
                .collect();
            for (column, id) in found {
                let point = Point::new(line, column);
                grid[point].set_reflow_anchor(None);
                mapped.insert(id, point);
            }
        }
        self.map_points(|point| {
            let old = anchor_cell(point);
            let id = cells.get(&old)?;
            let mut target = *mapped.get(id)?;
            target.column += point.column.0 - old.column.0;
            Some(target)
        });
    }
}

impl<T> Term<T> {
    /// Return the scroll delta for a previous/next prompt, including prompts on the same screen.
    pub fn shell_prompt_scroll(&mut self, previous: bool) -> Option<i32> {
        if self.mode.contains(TermMode::ALT_SCREEN) {
            return None;
        }
        self.shell.flush_scroll();
        let current = self.shell.navigation;
        let top = Line(-(self.grid.display_offset() as i32));
        let index = if previous {
            let limit = current.unwrap_or(self.shell.commands.len());
            (0..limit).rev().find(|&i| {
                self.shell.commands[i].prompt.is_some_and(|p| {
                    p.line >= self.grid.topmost_line()
                        && (current.is_some() || self.shell.commands[i].output.is_some())
                        && (current.is_some() || self.grid.display_offset() == 0 || p.line < top)
                })
            })
        } else {
            let start = current.map_or(0, |i| i + 1);
            (start..self.shell.commands.len()).find(|&i| {
                self.shell.commands[i].prompt.is_some_and(|p| {
                    p.line >= self.grid.topmost_line() && (current.is_some() || p.line > top)
                })
            })
        }?;
        self.shell.navigation = Some(index);
        let point = self.shell.commands[index].prompt?;
        Some((-point.line.0).max(0) - self.grid.display_offset() as i32)
    }

    /// Inclusive bounds of the selected command's output, or the last completed command.
    pub fn command_output_range(&self) -> Option<(Point, Point)> {
        if self.mode.contains(TermMode::ALT_SCREEN) {
            return None;
        }
        let command = match self.shell.navigation {
            Some(index) => self.shell.commands.get(index)?,
            None => self.shell.commands.iter().rev().find(|command| command.complete)?,
        };
        let mut start = self.shell.effective(command.output?)?;
        if start.column >= self.columns() {
            start.line += 1;
            start.column = Column(0);
        }
        let mut end = self.shell.effective(command.end?)?;
        if !command.complete || start >= end || start.line < self.topmost_line() {
            return None;
        }
        if end.column == 0 {
            end.line -= 1;
            end.column = self.last_column();
        } else {
            end.column -= 1;
        }
        (end.line <= self.bottommost_line() && start <= end).then_some((start, end))
    }

    pub fn command_output_to_string(&self) -> Option<String> {
        let (start, end) = self.command_output_range()?;
        Some(self.bounds_to_string(start, end))
    }
}

impl<T: EventListener> Term<T> {
    pub(super) fn handle_shell_marker(&mut self, marker: u8, status: Option<u32>) {
        if self.mode.contains(TermMode::ALT_SCREEN) {
            return;
        }
        self.shell.flush_scroll();
        let mut point = self.grid.cursor.point;
        if self.grid.cursor.input_needs_wrap {
            point.column += 1;
        }
        match marker {
            b'A' => {
                self.shell.running = None;
                self.shell.navigation = None;
                // Prompt redraws should not create duplicate commands.
                if self.shell.commands.back().is_some_and(|c| !c.complete && c.output.is_none()) {
                    self.shell.commands.pop_back();
                }
                if self.shell.commands.len() == MAX_COMMANDS {
                    self.shell.commands.pop_front();
                }
                self.shell
                    .commands
                    .push_back(Command { prompt: Some(point), ..Command::default() });
            },
            b'C' => {
                let Some(command) = self.shell.commands.back_mut() else { return };
                if command.output.is_some() || command.complete {
                    return;
                }
                command.output = Some(point);
                self.shell.running = Some(Instant::now());
            },
            b'D' => {
                let Some(started) = self.shell.running.take() else { return };
                let Some(command) = self.shell.commands.back_mut() else { return };
                command.end = Some(point);
                command.complete = true;
                let elapsed = started.elapsed();
                // Short commands never need a UI event. The configurable UI threshold can be higher.
                if elapsed >= Duration::from_secs(1) {
                    self.event_proxy.send_event(Event::CommandFinished { status, elapsed });
                }
            },
            _ => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::VoidListener;
    use crate::grid::Scroll;
    use crate::term::Config;
    use crate::term::test::TermSize;
    use crate::vte::ansi::{ClearMode, Handler, Processor};

    fn terminal(columns: usize, lines: usize, history: usize) -> Term<VoidListener> {
        Term::new(
            Config { scrolling_history: history, ..Config::default() },
            &TermSize::new(columns, lines),
            VoidListener,
        )
    }

    fn feed<T: EventListener>(term: &mut Term<T>, text: &str) {
        let mut parser: Processor = Processor::new();
        // Split every escape and multibyte character across reads.
        for byte in text.as_bytes() {
            parser.advance(term, &[*byte]);
        }
    }

    fn command(term: &mut Term<VoidListener>, output: &str) {
        feed(term, "\x1b]133;A\x07$ run\x1b]133;B\x07\r\n\x1b]133;C\x07");
        feed(term, output);
        feed(term, "\x1b]133;D;0\x1b\\");
    }

    #[test]
    fn output_excludes_prompt_and_command_with_unicode_and_wrap() {
        let mut term = terminal(8, 5, 100);
        command(&mut term, "hello 中文 world\r\nsecond line\r\n");
        assert_eq!(
            term.command_output_to_string().as_deref(),
            Some("hello 中文 world\nsecond line")
        );
        feed(&mut term, "\x1b]133;A\x07NEXT> \x1b]133;B\x07");
        assert_eq!(
            term.command_output_to_string().as_deref(),
            Some("hello 中文 world\nsecond line")
        );
    }

    #[test]
    fn output_survives_reflow_and_height_changes() {
        let mut term = terminal(14, 6, 100);
        command(&mut term, "abc中文123456789\r\nsecond\r\n");
        let expected = term.command_output_to_string().unwrap();
        for (cols, lines) in [(7, 4), (25, 8), (4, 5), (14, 6)] {
            term.resize(TermSize::new(cols, lines));
            assert_eq!(
                term.command_output_to_string().as_deref(),
                Some(expected.as_str()),
                "size {cols}x{lines}"
            );
            for row in term.topmost_line().0..=term.bottommost_line().0 {
                assert!(term.grid[Line(row)][..].iter().all(|cell| cell.reflow_anchor().is_none()));
            }
        }
    }

    #[test]
    fn reflow_anchors_do_not_change_terminal_content() {
        for output in [
            "",
            "12345678",
            "中文中文中文",
            "one\r\n\r\nthree\r\n",
            "a long wrapped output with many words\r\n",
        ] {
            for columns in 2..=24 {
                let mut marked = terminal(8, 6, 100);
                let mut plain = terminal(8, 6, 100);
                command(&mut marked, output);
                command(&mut plain, output);
                plain.shell = ShellState::default();
                marked.resize(TermSize::new(columns, 4));
                plain.resize(TermSize::new(columns, 4));
                assert_eq!(
                    marked.grid.cursor, plain.grid.cursor,
                    "output {output:?}, columns {columns}"
                );
                assert_eq!(
                    marked.history_size(),
                    plain.history_size(),
                    "output {output:?}, columns {columns}"
                );
                for row in plain.topmost_line().0..=plain.bottommost_line().0 {
                    assert_eq!(
                        marked.grid[Line(row)],
                        plain.grid[Line(row)],
                        "output {output:?}, columns {columns}, row {row}"
                    );
                }
            }
        }
    }

    #[test]
    fn navigation_selects_previous_commands_even_on_same_screen() {
        let mut term = terminal(20, 15, 100);
        command(&mut term, "first\r\n");
        command(&mut term, "second\r\n");
        feed(&mut term, "\x1b]133;A\x07$ ");
        for expected in ["second", "first"] {
            let delta = term.shell_prompt_scroll(true).unwrap();
            term.scroll_display(Scroll::Delta(delta));
            assert_eq!(term.command_output_to_string().as_deref(), Some(expected));
        }
        assert!(term.shell_prompt_scroll(true).is_none());
        term.shell_prompt_scroll(false).unwrap();
        assert_eq!(term.command_output_to_string().as_deref(), Some("second"));
    }

    #[test]
    fn scrolling_eviction_and_clear_never_copy_unrelated_cells() {
        let mut term = terminal(20, 3, 4);
        command(&mut term, "old\r\n");
        for _ in 0..12 {
            feed(&mut term, "new\r\n");
        }
        assert!(term.command_output_to_string().is_none());
        command(&mut term, "recent\r\n");
        assert_eq!(term.command_output_to_string().as_deref(), Some("recent"));
        term.clear_screen(ClearMode::All);
        assert_eq!(term.command_output_to_string().as_deref(), Some("recent"));
        term.clear_screen(ClearMode::Saved);
        assert!(term.command_output_to_string().is_none());
    }

    #[test]
    fn alt_screen_and_unpaired_or_invalid_markers_are_ignored() {
        let mut term = terminal(20, 5, 100);
        feed(&mut term, "\x1b]133;C\x07bad\x1b]133;D;0\x07");
        assert!(term.command_output_to_string().is_none());
        command(&mut term, "saved\r\n");
        term.swap_alt();
        command(&mut term, "alternate\r\n");
        assert!(term.command_output_to_string().is_none());
        term.resize(TermSize::new(10, 5));
        term.swap_alt();
        assert_eq!(term.command_output_to_string().as_deref(), Some("saved"));
        feed(&mut term, "\x1b]133;D;999\x07");
        assert_eq!(term.command_output_to_string().as_deref(), Some("saved"));
        feed(&mut term, "\x1b]133;A\x07$ run\r\n\x1b]133;C\x07live\x1b]133;D;bad\x07");
        assert!(term.shell.running.is_some());
    }

    #[test]
    fn empty_output_and_pending_wrap_endpoints() {
        let mut term = terminal(8, 5, 100);
        command(&mut term, "");
        assert!(term.command_output_to_string().is_none());
        command(&mut term, "12345678");
        assert_eq!(term.command_output_to_string().as_deref(), Some("12345678"));
        term.resize(TermSize::new(5, 5));
        assert_eq!(term.command_output_to_string().as_deref(), Some("12345678"));
    }

    #[test]
    fn completion_reports_once_with_status_and_duration() {
        use std::sync::{Arc, Mutex};
        #[derive(Clone)]
        struct Listener(Arc<Mutex<Vec<Event>>>);
        impl EventListener for Listener {
            fn send_event(&self, event: Event) {
                self.0.lock().unwrap().push(event);
            }
        }
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut term =
            Term::new(Config::default(), &TermSize::new(20, 5), Listener(events.clone()));
        feed(&mut term, "\x1b]133;A\x07$ run\r\n\x1b]133;C\x07");
        term.shell.running = Some(Instant::now() - Duration::from_secs(12));
        feed(&mut term, "\x1b]133;D;7\x07\x1b]133;D;7\x07\x1b]133;A\x07");
        let events = events.lock().unwrap();
        let completions: Vec<_> =
            events.iter().filter(|e| matches!(e, Event::CommandFinished { .. })).collect();
        assert_eq!(completions.len(), 1);
        assert!(
            matches!(completions[0], Event::CommandFinished { status: Some(7), elapsed } if elapsed.as_secs() >= 12)
        );
    }
}
