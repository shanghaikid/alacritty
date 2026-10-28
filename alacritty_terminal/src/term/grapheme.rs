//! Incremental extended grapheme assembly, independent of PTY read boundaries.

use super::cell::CellExtra;
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{Cell, Column, EventListener, Flags, Point, Term, TermMode};
use crate::grid::Dimensions;

/// CJK letters, punctuation and precomposed Hangul syllables that start
/// separate graphemes after ASCII or another member of this set. Combining
/// kana marks, tone marks and conjoining Hangul jamo deliberately stay out.
pub(super) fn independent(c: char) -> bool {
    matches!(c as u32, 0x3000..=0x3029 | 0x3030..=0x3098 | 0x309b..=0x30ff
        | 0x3100..=0x312f | 0x31f0..=0x31ff | 0x3400..=0x9fff
        | 0xac00..=0xd7a3 | 0xf900..=0xfaff | 0xff01..=0xff60 | 0x20000..=0x3ffff)
}

#[derive(Clone, Copy, Default)]
struct ClusterEntry {
    text: [u32; 4],
    len: u8,
    // Zero means more than one grapheme; otherwise the clamped cell width.
    width: u8,
}

/// Unicode properties depend only on text, not on the current grid or styles.
/// Keep a small direct-mapped cache for repeated accents and emoji. Keys are
/// checked in full, collisions overwrite, and long clusters use the same
/// uncached algorithm. Allocation is lazy so ASCII-only terminals pay nothing.
#[derive(Default)]
pub(super) struct ClusterCache {
    entries: Option<Box<[ClusterEntry; 128]>>,
    tails: Option<Box<[Option<Arc<CellExtra>>; 128]>>,
}

impl ClusterCache {
    /// Share short immutable grapheme tails. The base letter and display flags
    /// stay on the cell, so different accented letters reuse the same tail.
    fn append(&mut self, cell: &mut Cell, next: char) {
        let tail = cell.zerowidth().unwrap_or_default();
        if tail.len() >= 4
            || cell.underline_color().is_some()
            || cell.extra.as_ref().is_some_and(|extra| extra.hyperlink().is_some())
        {
            cell.push_zerowidth(next);
            return;
        }
        let mut text = ['\0'; 4];
        text[..tail.len()].copy_from_slice(tail);
        text[tail.len()] = next;
        let text = &text[..tail.len() + 1];
        let hash = text.iter().fold(0u32, |hash, &c| (hash ^ c as u32).wrapping_mul(16777619));
        let index = ((hash ^ (hash >> 16)) as usize) & 127;
        let entries = self.tails.get_or_insert_with(|| Box::new(std::array::from_fn(|_| None)));
        if let Some(extra) = &entries[index] {
            if extra.zerowidth() == text {
                cell.extra = Some(extra.clone());
                return;
            }
        }
        cell.push_zerowidth(next);
        entries[index] = cell.extra.clone();
    }

    fn width(
        &mut self,
        base: char,
        tail: &[char],
        next: char,
        buffer: &mut String,
    ) -> Option<usize> {
        // These Unicode rules cover whole classes, independent of the concrete
        // letters. Avoid filling the text cache with every accent/flag spelling.
        // U+0300..036F are Extend characters with zero terminal width.
        if base.is_ascii_graphic()
            && ('\u{300}'..='\u{36f}').contains(&next)
            && tail.iter().all(|c| ('\u{300}'..='\u{36f}').contains(c))
        {
            return Some(1);
        }
        // GB12/GB13: two regional indicators form a flag, while a third
        // starts the next cluster. Other tails still use the full algorithm.
        if ('\u{1f1e6}'..='\u{1f1ff}').contains(&next) {
            if tail.is_empty() && ('\u{1f1e6}'..='\u{1f1ff}').contains(&base) {
                return Some(2);
            }
            if tail.is_empty() && base.is_ascii() {
                return None;
            }
        }
        let mut key = ClusterEntry::default();
        let index = if tail.len() <= 2 {
            key.len = (tail.len() + 2) as u8;
            key.text[0] = base as u32;
            for (dest, &c) in key.text[1..].iter_mut().zip(tail) {
                *dest = c as u32;
            }
            key.text[key.len as usize - 1] = next as u32;
            let mut hash = key.len as u32;
            for &c in &key.text[..key.len as usize] {
                hash = (hash ^ c).wrapping_mul(16777619);
            }
            let index = ((hash ^ (hash >> 16)) as usize) & 127;
            if let Some(entries) = &self.entries {
                let entry = &entries[index];
                if entry.len == key.len && entry.text == key.text {
                    return (entry.width != 0).then_some(entry.width as usize);
                }
            }
            Some(index)
        } else {
            None
        };

        buffer.clear();
        buffer.push(base);
        buffer.extend(tail);
        buffer.push(next);
        let width = if buffer.graphemes(true).next().unwrap().len() == buffer.len() {
            Some(buffer.width().clamp(1, 2))
        } else {
            None
        };
        if let Some(index) = index {
            key.width = width.unwrap_or(0) as u8;
            self.entries.get_or_insert_with(|| Box::new([ClusterEntry::default(); 128]))[index] =
                key;
        }
        width
    }
}

impl<T: EventListener> Term<T> {
    /// A CJK run may bypass clustering only when its first character cannot
    /// attach to the preceding cell, including when autowrap is pending.
    pub(super) fn can_start_wide_run(&self) -> bool {
        if !self.config.grapheme_clusters {
            return true;
        }
        let mut point = self.grid.cursor.point;
        if point.column == 0 && !self.grid.cursor.input_needs_wrap {
            return true;
        }
        if !self.grid.cursor.input_needs_wrap {
            point.column -= 1;
        }
        if self.grid[point].flags.contains(Flags::WIDE_CHAR_SPACER) {
            point.column = Column(point.column.saturating_sub(1));
        }
        let cell = &self.grid[point];
        (cell.c.is_ascii() || independent(cell.c))
            && cell.zerowidth().is_none_or(<[char]>::is_empty)
    }

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
        // Common CJK text does not need the full grapheme state machine.
        if independent(character)
            && (cell.c.is_ascii() || independent(cell.c))
            && cell.zerowidth().is_none_or(<[char]>::is_empty)
        {
            return false;
        }
        let Some(width) = self.grapheme_cache.width(
            cell.c,
            cell.zerowidth().unwrap_or_default(),
            character,
            &mut self.grapheme_buffer,
        ) else {
            return false;
        };

        let old_width = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
        self.grid[origin].flags.insert(Flags::GRAPHEME);
        self.damage.damage_line(origin.line.0 as usize, origin.column.0, cursor.column.0);

        // A variation selector can turn a one-column symbol at the right margin
        // into a wide emoji. Move the complete cluster, preserving its attributes.
        if width > old_width && origin.column == self.last_column() {
            self.grapheme_cache.append(&mut self.grid[origin], character);
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

        self.grapheme_cache.append(&mut self.grid[origin], character);
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

    #[test]
    fn shared_tails_preserve_cells_snapshots_and_extra_attributes() {
        let mut cache = ClusterCache::default();
        let mut snapshots = Vec::new();
        for text in
            ["\u{301}", "\u{200d}💻", "\u{200d}⚕\u{fe0f}", "\u{301}\u{302}\u{303}\u{304}\u{305}"]
        {
            for styled in [false, true] {
                let mut cell = Cell::default();
                if styled {
                    cell.set_underline_color(Some(crate::vte::ansi::Color::Indexed(17)));
                }
                let mut expected = cell.clone();
                for c in text.chars() {
                    cache.append(&mut cell, c);
                    expected.push_zerowidth(c);
                    assert_eq!(cell, expected);
                    snapshots.push((cell.clone(), expected.clone()));
                }
            }
        }
        for (actual, expected) in snapshots {
            assert_eq!(actual, expected);
        }
        let mut a = Cell::default();
        let mut b = Cell::default();
        cache.append(&mut a, '\u{301}');
        cache.append(&mut b, '\u{301}');
        assert!(Arc::ptr_eq(a.extra.as_ref().unwrap(), b.extra.as_ref().unwrap()));
        a.push_zerowidth('\u{302}');
        assert_eq!(b.zerowidth().unwrap(), &['\u{301}']);
    }

    #[test]
    fn accent_and_flag_rules_match_unicode_tables() {
        let mut cache = ClusterCache::default();
        let mut scratch = String::new();
        let mut verify = |base: char, tail: &[char], next: char| {
            let text: String = std::iter::once(base)
                .chain(tail.iter().copied())
                .chain(std::iter::once(next))
                .collect();
            let expected = (text.graphemes(true).count() == 1).then(|| text.width().clamp(1, 2));
            assert_eq!(cache.width(base, tail, next, &mut scratch), expected, "{text:?}");
        };
        for base in ('!'..='~').chain('\u{1f1e6}'..='\u{1f1ff}') {
            for next in ('\u{300}'..='\u{36f}').chain('\u{1f1e6}'..='\u{1f1ff}') {
                for tail in
                    [&[][..], &['\u{301}'][..], &['\u{301}', '\u{34f}'][..], &['\u{1f1e6}'][..]]
                {
                    verify(base, tail, next);
                }
            }
        }
    }

    #[test]
    fn cluster_cache_matches_uncached_segmentation_and_width() {
        let alphabet: Vec<char> =
            "ab中한👩💻🇨🇳❤️👍🏽\u{301}\u{308}\u{200d}\u{200b}\u{600}\u{1100}\u{1161}\u{11a8}"
                .chars()
                .collect();
        let mut cache = ClusterCache::default();
        let mut scratch = String::new();
        let mut seed = 11u64;
        for _ in 0..10_000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let len = 2 + (seed >> 32) as usize % 8;
            let mut chars = Vec::new();
            for _ in 0..len {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                chars.push(alphabet[(seed >> 32) as usize % alphabet.len()]);
            }
            let text: String = chars.iter().collect();
            let expected = (text.graphemes(true).count() == 1).then(|| text.width().clamp(1, 2));
            for _ in 0..2 {
                assert_eq!(
                    cache.width(chars[0], &chars[1..len - 1], chars[len - 1], &mut scratch),
                    expected,
                    "{text:?}"
                );
            }
        }
        assert_eq!(cache.entries.as_ref().unwrap().len(), 128);
    }

    #[test]
    fn independent_ranges_match_unicode_grapheme_boundaries() {
        let mut text = String::new();
        // Cover every accepted codepoint with the distinct preceding classes
        // admitted by the fast path (Any, Extended_Pictographic, LV and LVT).
        for c in (0..=0x3ffff).filter_map(char::from_u32).filter(|&c| independent(c)) {
            for previous in ['a', '中', '〰', '가', '각', c] {
                text.clear();
                text.push(previous);
                text.push(c);
                assert_eq!(text.graphemes(true).count(), 2, "{previous:?} + {c:?}");
            }
        }
    }

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
