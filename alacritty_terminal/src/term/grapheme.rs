//! Incremental extended grapheme assembly, independent of PTY read boundaries.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{Cell, Column, EventListener, Flags, Point, Term, TermMode};
use crate::grid::Dimensions;

impl<T: EventListener> Term<T> {
    /// Append a scalar to the previous cell only when it extends its grapheme.
    pub(super) fn join_grapheme(&mut self, character: char) -> bool {
        let cursor = self.grid.cursor.point;
        if cursor.column == 0 && !self.grid.cursor.input_needs_wrap {
            return false;
        }
        let mut origin = cursor;
        if !self.grid.cursor.input_needs_wrap {
            origin.column -= 1;
        }
        if self.grid[origin].flags.contains(Flags::WIDE_CHAR_SPACER) {
            origin.column = Column(origin.column.saturating_sub(1));
        }
        let cell = &self.grid[origin];
        // Ordinary ideographs cannot extend an ASCII/ideograph grapheme with
        // no tail. Keep the common Chinese output path free of segmentation.
        let ideograph = |c: char| matches!(c as u32, 0x3400..=0x9fff | 0x20000..=0x3ffff);
        if ideograph(character)
            && (cell.c.is_ascii() || ideograph(cell.c))
            && cell.zerowidth().is_none_or(<[char]>::is_empty)
        {
            return false;
        }
        self.grapheme_buffer.clear();
        self.grapheme_buffer.push(cell.c);
        self.grapheme_buffer.extend(cell.zerowidth().into_iter().flatten());
        self.grapheme_buffer.push(character);
        let mut graphemes = self.grapheme_buffer.graphemes(true);
        graphemes.next();
        if graphemes.next().is_some() {
            return false;
        }

        let old_width = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
        let width = self.grapheme_buffer.width().clamp(1, 2);
        self.grid[origin].flags.insert(Flags::GRAPHEME);
        self.damage.damage_line(origin.line.0 as usize, origin.column.0, cursor.column.0);

        // A variation selector can turn a one-column symbol at the right margin
        // into a wide emoji. Move the complete cluster, preserving its attributes.
        if width > old_width && origin.column == self.last_column() {
            self.grid[origin].push_zerowidth(character);
            if !self.mode.contains(TermMode::LINE_WRAP) {
                // Like a wide scalar at the margin with DECAWM off, keep the
                // cursor at the margin. Preserve the text for selection/copy.
                return true;
            }
            let mut cluster = self.grid[origin].clone();
            self.grid[origin].clear_wide();
            self.grid[origin].flags.insert(Flags::LEADING_WIDE_CHAR_SPACER);
            self.wrapline();
            origin = self.grid.cursor.point;
            cluster.flags.remove(Flags::WRAPLINE | Flags::LEADING_WIDE_CHAR_SPACER);
            cluster.flags.insert(Flags::WIDE_CHAR);
            // Clear the destination's old wide pair before installing the new
            // cluster; otherwise its old spacer would erase the new base.
            self.write_at_cursor(cluster.c);
            self.grid[origin] = cluster;
            self.write_grapheme_spacer(origin);
            self.advance_past_grapheme(origin, width);
            return true;
        }

        self.grid[origin].push_zerowidth(character);
        if width > old_width {
            // Insert mode needs one additional column for the enlarged cluster.
            if self.mode.contains(TermMode::INSERT) {
                let columns = self.columns();
                let row = &mut self.grid[origin.line][..];
                for col in (origin.column.0 + 1..columns - 1).rev() {
                    row.swap(col, col + 1);
                }
            }
            self.grid[origin].flags.insert(Flags::WIDE_CHAR);
            self.write_grapheme_spacer(origin);
            self.advance_past_grapheme(origin, width);
        } else if width < old_width {
            self.grid[origin].flags.remove(Flags::WIDE_CHAR);
            let spacer = Point::new(origin.line, origin.column + 1);
            self.grid[spacer] = Cell { bg: self.grid[origin].bg, ..Cell::default() };
            self.advance_past_grapheme(origin, width);
        }
        true
    }

    fn write_grapheme_spacer(&mut self, origin: Point) {
        let point = Point::new(origin.line, origin.column + 1);
        let mut spacer = self.grid[origin].clone();
        spacer.clear_wide();
        spacer.flags.remove(Flags::WRAPLINE | Flags::LEADING_WIDE_CHAR_SPACER);
        spacer.flags.insert(Flags::WIDE_CHAR_SPACER);
        // Use the normal overwrite path to clear any existing wide character.
        let cursor = self.grid.cursor.point;
        self.grid.cursor.point = point;
        self.write_at_cursor(' ');
        self.grid.cursor.point = cursor;
        self.grid[point] = spacer;
    }

    fn advance_past_grapheme(&mut self, origin: Point, width: usize) {
        let next = origin.column + width;
        self.grid.cursor.point = Point::new(origin.line, next.min(self.last_column()));
        self.grid.cursor.input_needs_wrap = next > self.last_column();
        self.damage_cursor();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::VoidListener;
    use crate::index::Line;
    use crate::term::Config;
    use crate::term::test::TermSize;
    use crate::vte::ansi::{Handler, Processor};

    fn terminal(columns: usize) -> Term<VoidListener> {
        let config = Config { grapheme_clusters: true, ..Config::default() };
        Term::new(config, &TermSize::new(columns, 3), VoidListener)
    }

    #[test]
    fn emoji_clusters_keep_their_text_and_occupy_two_columns() {
        for text in ["👩‍💻", "🇨🇳", "👍🏽", "❤️", "1️⃣", "👨‍👩‍👧‍👦"]
        {
            let mut term = terminal(12);
            let mut parser: Processor = Processor::new();
            // Every UTF-8 byte arrives independently, including inside scalars.
            for byte in text.as_bytes() {
                parser.advance(&mut term, &[*byte]);
            }
            let cell = &term.grid[Line(0)][Column(0)];
            assert!(cell.flags.contains(Flags::WIDE_CHAR), "{text}");
            assert!(term.grid[Line(0)][Column(1)].flags.contains(Flags::WIDE_CHAR_SPACER));
            let actual: String =
                std::iter::once(cell.c).chain(cell.zerowidth().unwrap().iter().copied()).collect();
            assert_eq!(actual, text);
            assert_eq!(term.grid.cursor.point.column, Column(2), "{text}");
            term.input('x');
            assert_eq!(term.grid[Line(0)][Column(2)].c, 'x');
        }
    }

    #[test]
    fn growth_at_margin_moves_the_complete_cluster() {
        let mut term = terminal(4);
        for c in "abc❤️x".chars() {
            term.input(c);
        }
        assert!(term.grid[Line(0)][Column(3)].flags.contains(Flags::LEADING_WIDE_CHAR_SPACER));
        assert!(term.grid[Line(0)][Column(3)].flags.contains(Flags::WRAPLINE));
        assert_eq!(term.grid[Line(1)][Column(0)].c, '❤');
        assert_eq!(term.grid[Line(1)][Column(0)].zerowidth(), Some(&['\u{fe0f}'][..]));
        assert!(term.grid[Line(1)][Column(1)].flags.contains(Flags::WIDE_CHAR_SPACER));
        assert_eq!(term.grid[Line(1)][Column(2)].c, 'x');
    }

    #[test]
    fn growth_at_margin_overwrites_an_existing_wide_character() {
        let mut term = terminal(4);
        term.goto(1, 0);
        term.input('中');
        term.goto(0, 3);
        for c in "❤️".chars() {
            term.input(c);
        }
        let cell = &term.grid[Line(1)][Column(0)];
        assert_eq!(cell.c, '❤');
        assert_eq!(cell.zerowidth(), Some(&['\u{fe0f}'][..]));
        assert!(cell.flags.contains(Flags::WIDE_CHAR | Flags::GRAPHEME));
        assert!(term.grid[Line(1)][Column(1)].flags.contains(Flags::WIDE_CHAR_SPACER));
    }

    #[test]
    fn widened_cluster_spacer_preserves_display_attributes() {
        let mut term = terminal(4);
        term.grid.cursor.template.flags.insert(Flags::INVERSE | Flags::UNDERLINE);
        for c in "❤️".chars() {
            term.input(c);
        }
        let spacer = &term.grid[Line(0)][Column(1)];
        assert!(spacer.flags.contains(Flags::INVERSE | Flags::UNDERLINE | Flags::WIDE_CHAR_SPACER));
        assert!(!spacer.flags.intersects(Flags::WIDE_CHAR | Flags::GRAPHEME));
        assert!(spacer.zerowidth().is_none_or(<[char]>::is_empty));
    }

    #[test]
    fn adjacent_flags_are_separate_clusters() {
        let mut term = terminal(12);
        for c in "🇨🇳🇺🇸中文e\u{301}".chars() {
            term.input(c);
        }
        assert_eq!(term.grid[Line(0)][Column(0)].zerowidth(), Some(&['🇳'][..]));
        assert_eq!(term.grid[Line(0)][Column(2)].zerowidth(), Some(&['🇸'][..]));
        assert_eq!(term.grid[Line(0)][Column(4)].c, '中');
        assert_eq!(term.grid[Line(0)][Column(6)].c, '文');
        assert_eq!(term.grid[Line(0)][Column(8)].c, 'e');
        assert_eq!(term.grid.cursor.point.column, Column(9));
    }

    #[test]
    fn clusters_survive_copy_resize_and_search_in_both_directions() {
        use crate::index::{Direction, Side};
        use crate::term::search::RegexSearch;

        let mut term = terminal(20);
        let text = "A👩‍💻🇨🇳👍🏽❤️B";
        for c in text.chars() {
            term.input(c);
        }
        let start = Point::new(Line(0), Column(0));
        let end = Point::new(Line(0), Column(9));
        assert_eq!(term.bounds_to_string(start, end), text);
        for pattern in ["👩‍💻", "🇨🇳", "👍🏽", "❤️", "💻", "👩‍💻🇨🇳"]
        {
            let mut regex = RegexSearch::new(pattern).unwrap();
            let right = term.search_next(&mut regex, start, Direction::Right, Side::Left, None);
            let left = term.search_next(&mut regex, end, Direction::Left, Side::Right, None);
            assert!(right.is_some(), "{pattern}");
            assert_eq!(right, left, "{pattern}");
        }
        term.resize(TermSize::new(6, 5));
        term.resize(TermSize::new(20, 3));
        let all_text = term.bounds_to_string(
            Point::new(term.topmost_line(), Column(0)),
            Point::new(term.bottommost_line(), term.last_column()),
        );
        assert_eq!(all_text.trim(), text);
    }

    #[test]
    fn cached_search_observes_a_changed_cluster_tail() {
        use crate::term::search::RegexSearch;

        let mut term = terminal(12);
        for c in "👍🏽".chars() {
            term.input(c);
        }
        let start = Point::new(Line(0), Column(0));
        let end = Point::new(Line(0), Column(11));
        let mut regex = RegexSearch::new("👍🏽").unwrap();
        assert_eq!(regex.cached_matches(&term, start, end, Line(0)).len(), 1);
        term.goto(0, 0);
        for c in "👍🏻".chars() {
            term.input(c);
        }
        assert!(regex.cached_matches(&term, start, end, Line(0)).is_empty());
    }

    #[test]
    fn scalar_and_cluster_searches_agree_with_unicode_regex() {
        use crate::index::Direction;
        use crate::term::search::{RegexIter, RegexSearch};

        let text = "A👩‍💻🇨🇳👍🏽❤️e\u{301}B";
        let mut term = terminal(text.width());
        for c in text.chars() {
            term.input(c);
        }
        let mut byte_cells = Vec::new();
        let mut column = 0;
        for cluster in text.graphemes(true) {
            let range = Point::new(Line(0), Column(column))
                ..=Point::new(Line(0), Column(column + cluster.width() - 1));
            byte_cells.extend(std::iter::repeat_n(range, cluster.len()));
            column += cluster.width();
        }
        let start = Point::new(Line(0), Column(0));
        let end = Point::new(Line(0), Column(column - 1));
        for pattern in [
            "👩",
            "💻",
            "👩‍💻",
            "🇨",
            "🇳",
            "🏽",
            "❤️",
            "❤",
            "e",
            "\u{301}",
            "👩.*❤️",
            ".",
            "..",
            ".*",
            "^A",
            "B$",
            "👩|💻",
        ] {
            let reference = regex_automata::meta::Regex::new(pattern).unwrap();
            let mut expected: Vec<_> = reference
                .find_iter(text)
                .filter(|m| !m.is_empty())
                .map(|m| *byte_cells[m.start()].start()..=*byte_cells[m.end() - 1].end())
                .collect();
            expected.dedup();
            // Terminal search advances by whole cells after a match; multiple
            // scalar matches inside an already highlighted cluster coalesce.
            let mut previous_end = None;
            expected.retain(|range| {
                if previous_end.is_some_and(|end| *range.start() <= end) {
                    return false;
                }
                previous_end = Some(*range.end());
                true
            });
            let mut regex = RegexSearch::new(pattern).unwrap();
            let actual: Vec<_> =
                RegexIter::new(start, end, Direction::Right, &term, &mut regex).take(30).collect();
            assert_eq!(actual, expected, "forward {pattern}");
            // Directional searches must find the same cell ranges for literals.
            if !pattern.contains(['.', '|', '^', '$']) {
                let actual: Vec<_> = RegexIter::new(end, start, Direction::Left, &term, &mut regex)
                    .take(30)
                    .collect();
                expected.reverse();
                assert_eq!(actual, expected, "reverse {pattern}");
            }
        }
    }

    #[test]
    fn insert_mode_and_overwriting_keep_wide_spacers_consistent() {
        let mut term = terminal(12);
        for c in "abcd".chars() {
            term.input(c);
        }
        term.goto(0, 1);
        term.mode.insert(TermMode::INSERT);
        for c in "❤️".chars() {
            term.input(c);
        }
        assert_eq!(
            term.bounds_to_string(Point::new(Line(0), Column(0)), Point::new(Line(0), Column(5))),
            "a❤️bcd"
        );
        term.mode.remove(TermMode::INSERT);
        term.goto(0, 2);
        term.input('X');
        assert!(!term.grid[Line(0)][Column(1)].flags.contains(Flags::WIDE_CHAR));
        assert!(!term.grid[Line(0)][Column(2)].flags.contains(Flags::WIDE_CHAR_SPACER));
        assert_eq!(term.grid[Line(0)][Column(2)].c, 'X');
    }
}
