//! Cold rows keep a lossless encoding and expand on first access.
//!
//! OnceLock gives readers stable references without interior unsafe code. Only
//! exclusive access can discard an expanded cache or turn the row mutable.

use std::ops::{Deref, DerefMut, Range};
use std::sync::{Arc, OnceLock};

#[cfg(feature = "serde")]
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Debug)]
pub struct CompactRow<T> {
    pub(crate) block: Arc<CompactBlock<T>>,
    pub(crate) start: u32,
    pub(crate) len: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct CompactBlock<T> {
    pub(crate) templates: Vec<T>,
    pub(crate) bytes: Vec<u8>,
    pub(crate) decode: RowDecoder<T>,
    pub(crate) clone_cells: fn(&[T]) -> Vec<T>,
    pub(crate) len: usize,
    pub(crate) expanded: OnceLock<Vec<T>>,
}

type RowDecoder<T> = fn(&[T], &[u8], Range<usize>) -> Vec<T>;

/// Writable scalar storage has one owner. Keep its style inline instead of
/// allocating an Arc and a one-element style dictionary for every live row.
#[derive(Clone, Debug)]
pub struct PackedRow<T> {
    pub(crate) template: T,
    pub(crate) bytes: Vec<u8>,
    pub(crate) expanded: OnceLock<Vec<T>>,
    pub(crate) decode: RowDecoder<T>,
}

impl<T> PackedRow<T> {
    fn len(&self) -> usize {
        self.bytes.len() / 4
    }

    fn decode(&self) -> Vec<T> {
        (self.decode)(std::slice::from_ref(&self.template), &self.bytes, 0..self.len())
    }

    fn cells(&self) -> &Vec<T> {
        self.expanded.get_or_init(|| self.decode())
    }
}

impl<T> CompactRow<T> {
    fn range(&self) -> Range<usize> {
        self.start as usize..self.start as usize + self.len as usize
    }

    fn cells(&self) -> &[T] {
        let block = &self.block;
        let cells = block
            .expanded
            .get_or_init(|| (block.decode)(&block.templates, &block.bytes, 0..block.len));
        &cells[self.range()]
    }

    fn decode(&self) -> Vec<T> {
        // Mutating several rows in one block (notably during reflow) must not
        // decompress the same stream once per row. Share the first expansion
        // with the remaining rows, just as immutable access does.
        (self.block.clone_cells)(self.cells())
    }
}

#[derive(Clone, Debug)]
pub(super) enum Cells<T> {
    Dense(Vec<T>),
    /// Cheap, uncompressed history encoding; eligible for idle compression.
    Packed(Box<PackedRow<T>>),
    // A row is just a range in a shared block. One expansion cache per block
    // avoids a separate allocation and OnceLock for every cold history row.
    Compact(CompactRow<T>),
}

impl<T> Default for Cells<T> {
    fn default() -> Self {
        Self::Dense(Vec::new())
    }
}

impl<T> Cells<T> {
    pub(super) fn len(&self) -> usize {
        match self {
            Self::Dense(cells) => cells.len(),
            Self::Packed(row) => row.len(),
            Self::Compact(row) => row.len as usize,
        }
    }

    pub(super) fn compact(&mut self)
    where
        T: super::GridCell,
    {
        match self {
            Self::Dense(cells) => {
                if let Some(row) = T::compact_row(cells) {
                    *self = Self::Compact(row);
                }
            },
            Self::Compact(row) => {
                if row.block.expanded.get().is_some() {
                    if let Some(compact) = T::compact_row(row.cells()) {
                        *self = Self::Compact(compact);
                    }
                }
            },
            Self::Packed(row) => {
                if let Some(compact) = T::compact_row(row.cells()) {
                    *self = Self::Compact(compact);
                } else {
                    row.expanded.take();
                }
            },
        }
    }
}

impl<T> Deref for Cells<T> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Dense(cells) => cells,
            Self::Packed(row) => row.cells(),
            Self::Compact(row) => row.cells(),
        }
    }
}

impl<T> DerefMut for Cells<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.make_mut_vec()
    }
}

impl<T> Cells<T> {
    #[inline]
    pub(super) fn make_mut_vec(&mut self) -> &mut Vec<T> {
        match self {
            Self::Dense(cells) => cells,
            _ => self.expand_mut(),
        }
    }
}

impl<T> Cells<T> {
    // Keep materialization out of the per-cell mutable indexing path. A third
    // representation must not make every ordinary dense write an indirect call.
    #[cold]
    fn expand_mut(&mut self) -> &mut Vec<T> {
        let cells = match self {
            Self::Packed(row) => row.expanded.take().unwrap_or_else(|| row.decode()),
            Self::Compact(row) => row.decode(),
            Self::Dense(_) => unreachable!(),
        };
        *self = Self::Dense(cells);
        match self {
            Self::Dense(cells) => cells,
            _ => unreachable!(),
        }
    }
}

impl<T: PartialEq> PartialEq for Cells<T> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

// Keep the reference-test format independent of storage policy.
#[cfg(feature = "serde")]
impl<T: Serialize> Serialize for Cells<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.deref().serialize(serializer)
    }
}

#[cfg(feature = "serde")]
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Cells<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::deserialize(deserializer).map(Self::Dense)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::VoidListener;
    use crate::grid::GridCell;
    use crate::grid::{Dimensions, Scroll};
    use crate::index::{Column, Line};
    use crate::term::cell::{Cell, Flags, Hyperlink};
    use crate::term::test::TermSize;
    use crate::term::{Config, Term};
    use crate::vte::ansi::{Color, NamedColor, Processor};

    #[test]
    fn packed_cells_preserve_scalars_flags_and_snapshots() {
        let mut original = vec![Cell::default(); 120];
        for (i, cell) in original.iter_mut().enumerate() {
            cell.c = ['a', '中', '\u{10ffff}', '😀'][i % 4];
            cell.fg = Color::Spec(crate::vte::ansi::Rgb { r: 12, g: 43, b: 200 });
            cell.flags = Flags::BOLD | Flags::UNDERLINE;
            for (bit, flag) in [
                Flags::WRAPLINE,
                Flags::WIDE_CHAR,
                Flags::WIDE_CHAR_SPACER,
                Flags::LEADING_WIDE_CHAR_SPACER,
            ]
            .into_iter()
            .enumerate()
            {
                if i & (1 << bit) != 0 {
                    cell.flags.insert(flag);
                }
            }
        }
        let mut rows = Cell::pack_rows(&[&original, &original[..81]]).unwrap();
        let short = Cells::Packed(Box::new(rows.pop().unwrap()));
        let mut packed = Cells::Packed(Box::new(rows.pop().unwrap()));
        assert_eq!(&*short, &original[..81]);
        assert_eq!(&*packed, &original);
        let snapshot = packed.clone();
        packed[0].push_zerowidth('\u{301}');
        assert_eq!(&*snapshot, &original);
        assert!(matches!(packed, Cells::Dense(_)));
        original[0].fg = Color::Indexed(1);
        assert!(Cell::pack_rows(&[&original]).is_none());
        original[0] = original[16].clone();
        original[0].set_hyperlink(Some(Hyperlink::new(Some("test"), "https://example.com".into())));
        assert!(Cell::pack_rows(&[&original]).is_none());
    }

    #[test]
    fn active_history_is_packed_without_an_idle_pass() {
        let config = Config { scrolling_history: 3000, ..Config::default() };
        let mut term = Term::new(config, &TermSize::new(120, 20), VoidListener);
        let mut parser: Processor = Processor::new();
        for i in 0..4000 {
            let line = format!("{i:06} 中文{}\r\n", "logging text ".repeat(7));
            parser.advance(&mut term, line.as_bytes());
        }
        let grid = term.grid();
        assert_eq!(grid.history_size(), 3000);
        let dense =
            (grid.topmost_line().0..0).filter(|&line| grid.raw[Line(line)].is_dense()).count();
        assert!(dense <= 256 + 15, "{dense} dense history rows");
        // Decoding for a reader preserves the packed representation and row data.
        assert_eq!(grid[Line(-1)][Column(5)].c, '0');
        assert_eq!(grid[Line(-1000)][Column(5)].c, '1');
    }

    #[test]
    fn compressed_cells_preserve_attributes_and_mutations() {
        let mut original = vec![Cell::default(); 120];
        for (i, cell) in original.iter_mut().enumerate() {
            cell.c = char::from_u32(0x4e00 + i as u32).unwrap();
            if i % 3 == 0 {
                cell.flags = Flags::BOLD | Flags::GRAPHEME;
                cell.fg = Color::Indexed(45);
                cell.push_zerowidth('\u{301}');
                cell.set_hyperlink(Some(Hyperlink::new(
                    Some("same"),
                    "https://example.com".into(),
                )));
            }
        }
        original[119].flags.insert(Flags::WRAPLINE);
        let compact = Cell::compact_row(&original).unwrap();
        let mut cells = Cells::Compact(compact);
        assert_eq!(&*cells, &original);
        let snapshot = cells.clone();
        cells.compact();
        assert!(matches!(&cells, Cells::Compact(row) if row.block.expanded.get().is_none()));
        cells[0].c = 'x';
        assert_eq!(&*snapshot, &original);
        original[0].c = 'x';
        assert_eq!(&*cells, &original);
        assert!(matches!(cells, Cells::Dense(_)));
    }

    #[test]
    fn varied_and_long_grapheme_tails_roundtrip_in_shared_blocks() {
        let mut original = vec![vec![Cell::default(); 120]; 16];
        for (row_index, row) in original.iter_mut().enumerate() {
            for (column, cell) in row.iter_mut().enumerate() {
                let index = row_index * 120 + column;
                cell.c = char::from_u32(0x4e00 + index as u32).unwrap();
                if index % 7 == 0 {
                    cell.extra = Some(Arc::default());
                } else if index % 5 != 0 {
                    for n in 0..index % 6 + 1 {
                        cell.push_zerowidth(
                            char::from_u32(0x300 + ((index + n) % 112) as u32).unwrap(),
                        );
                    }
                }
                if index % 3 == 0 {
                    cell.set_underline_color(Some(crate::vte::ansi::Color::Indexed(42)));
                    cell.set_hyperlink(Some(Hyperlink::new(
                        Some("shared"),
                        "https://example.com".into(),
                    )));
                }
            }
        }
        for _ in 0..300 {
            original[3][4].push_zerowidth('\u{301}');
        }
        let refs: Vec<_> = original.iter().map(Vec::as_slice).collect();
        let encoded = Cell::compact_rows(&refs).unwrap();
        assert!(encoded.iter().all(|row| Arc::ptr_eq(&row.block, &encoded[0].block)));
        for (row, expected) in encoded.into_iter().zip(original) {
            let mut cells = Cells::Compact(row);
            assert_eq!(&*cells, &expected);
            let snapshot = cells.clone();
            cells[4].c = 'x';
            assert_eq!(&*snapshot, &expected);
        }
    }

    #[test]
    fn shared_block_cache_preserves_row_ranges_and_snapshots() {
        let first = vec![Cell { c: '中', ..Cell::default() }; 81];
        let second = vec![Cell { c: 'z', ..Cell::default() }; 120];
        let mut encoded = Cell::compact_rows(&[&first, &second]).unwrap();
        assert!(Arc::ptr_eq(&encoded[0].block, &encoded[1].block));
        let mut b = Cells::Compact(encoded.pop().unwrap());
        let mut a = Cells::Compact(encoded.pop().unwrap());
        assert_eq!(&*a, &first);
        assert!(matches!(&b, Cells::Compact(row) if row.block.expanded.get().is_some()));
        // Clearing a sibling's cache must not invalidate an outstanding view.
        let first_view = &*a;
        b.compact();
        assert_eq!(first_view, &first);
        assert_eq!(&*b, &second);
        a[0].c = 'x';
        drop(a);
        assert_eq!(&*b, &second);
    }

    #[test]
    fn cold_mutations_share_expansion_and_preserve_snapshots() {
        let original = vec![Cell { c: '中', ..Cell::default() }; 120];
        let encoded = Cell::compact_rows(&[&original, &original]).unwrap();
        let block = encoded[0].block.clone();
        assert!(block.expanded.get().is_none());
        let mut rows: Vec<_> = encoded.into_iter().map(Cells::Compact).collect();
        let snapshot = rows.clone();
        rows[0][0].c = 'a';
        assert!(block.expanded.get().is_some());
        rows[1][0].c = 'b';
        assert_eq!(rows[0][0].c, 'a');
        assert_eq!(rows[1][0].c, 'b');
        for row in &snapshot {
            assert_eq!(&**row, &original);
        }
    }

    #[test]
    fn compressed_history_matches_resident_after_resize_and_recycle() {
        let mut term = Term::new(Config::default(), &TermSize::new(120, 20), VoidListener);
        let mut parser: Processor = Processor::new();
        for i in 0..1200 {
            let line =
                format!("{i:06} \x1b[32m中文e\u{301}👩‍💻\x1b[0m {}\r\n", "log text ".repeat(8));
            parser.advance(&mut term, line.as_bytes());
        }
        let mut compact = term.grid().clone();
        let mut resident = compact.clone();
        // The live-history path may already have packed rows. Force the
        // reference grid to dense storage before testing reflow/recycling.
        for line in resident.topmost_line().0..=resident.bottommost_line().0 {
            let _ = &mut resident[Line(line)][..];
        }
        compact.scroll_display(Scroll::Delta(350));
        resident.scroll_display(Scroll::Delta(350));
        let mut cursor = 0;
        while compact.compact_history(&mut cursor) {}
        for (columns, lines) in [(81, 30), (160, 15), (43, 40), (120, 20)] {
            compact.resize(true, lines, columns);
            resident.resize(true, lines, columns);
            assert_eq!(compact.history_size(), resident.history_size());
            for line in compact.topmost_line().0..=compact.bottommost_line().0 {
                assert_eq!(compact[Line(line)], resident[Line(line)], "line {line}");
            }
            cursor = 0;
            while compact.compact_history(&mut cursor) {}
        }
        compact.cursor.template.bg = Color::Named(NamedColor::Blue);
        resident.cursor.template.bg = Color::Named(NamedColor::Blue);
        for _ in 0..1400 {
            compact.scroll_up(&(Line(0)..Line(20)), 1);
            resident.scroll_up(&(Line(0)..Line(20)), 1);
        }
        for line in compact.topmost_line().0..=compact.bottommost_line().0 {
            assert_eq!(compact[Line(line)], resident[Line(line)]);
        }
        assert_eq!(compact[Line(0)][Column(0)].bg, Color::Named(NamedColor::Blue));
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serialization_is_independent_of_compression() {
        let original = vec![Cell::default(); 120];
        let dense = Cells::Dense(original.clone());
        let compact = Cells::Compact(Cell::compact_row(&original).unwrap());
        let encoded = serde_json::to_string(&compact).unwrap();
        assert_eq!(encoded, serde_json::to_string(&dense).unwrap());
        let decoded: Cells<Cell> = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, dense);
    }
}
