use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use bitflags::bitflags;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::grid::{self, GridCell};
use crate::index::Column;
use crate::vte::ansi::{Color, Hyperlink as VteHyperlink, NamedColor};

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
    pub struct Flags: u16 {
        const INVERSE                   = 0b0000_0000_0000_0001;
        const BOLD                      = 0b0000_0000_0000_0010;
        const ITALIC                    = 0b0000_0000_0000_0100;
        const BOLD_ITALIC               = 0b0000_0000_0000_0110;
        const UNDERLINE                 = 0b0000_0000_0000_1000;
        const WRAPLINE                  = 0b0000_0000_0001_0000;
        const WIDE_CHAR                 = 0b0000_0000_0010_0000;
        const WIDE_CHAR_SPACER          = 0b0000_0000_0100_0000;
        const DIM                       = 0b0000_0000_1000_0000;
        const DIM_BOLD                  = 0b0000_0000_1000_0010;
        const HIDDEN                    = 0b0000_0001_0000_0000;
        const STRIKEOUT                 = 0b0000_0010_0000_0000;
        const LEADING_WIDE_CHAR_SPACER  = 0b0000_0100_0000_0000;
        const DOUBLE_UNDERLINE          = 0b0000_1000_0000_0000;
        const UNDERCURL                 = 0b0001_0000_0000_0000;
        const DOTTED_UNDERLINE          = 0b0010_0000_0000_0000;
        const DASHED_UNDERLINE          = 0b0100_0000_0000_0000;
        const GRAPHEME                  = 0b1000_0000_0000_0000;
        const ALL_UNDERLINES            = Self::UNDERLINE.bits() | Self::DOUBLE_UNDERLINE.bits()
                                        | Self::UNDERCURL.bits() | Self::DOTTED_UNDERLINE.bits()
                                        | Self::DASHED_UNDERLINE.bits();
    }
}

/// Counter for hyperlinks without explicit ID.
static HYPERLINK_ID_SUFFIX: AtomicU32 = AtomicU32::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Hyperlink {
    inner: Arc<HyperlinkInner>,
}

impl Hyperlink {
    pub fn new<T: ToString>(id: Option<T>, uri: String) -> Self {
        let inner = Arc::new(HyperlinkInner::new(id, uri));
        Self { inner }
    }

    pub fn id(&self) -> &str {
        &self.inner.id
    }

    pub fn uri(&self) -> &str {
        &self.inner.uri
    }
}

impl From<VteHyperlink> for Hyperlink {
    fn from(value: VteHyperlink) -> Self {
        Self::new(value.id, value.uri)
    }
}

impl From<Hyperlink> for VteHyperlink {
    fn from(val: Hyperlink) -> Self {
        VteHyperlink { id: Some(val.id().to_owned()), uri: val.uri().to_owned() }
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
struct HyperlinkInner {
    /// Identifier for the given hyperlink.
    id: String,

    /// Resource identifier of the hyperlink.
    uri: String,
}

impl HyperlinkInner {
    pub fn new<T: ToString>(id: Option<T>, uri: String) -> Self {
        let id = match id {
            Some(id) => id.to_string(),
            None => {
                let mut id = HYPERLINK_ID_SUFFIX.fetch_add(1, Ordering::Relaxed).to_string();
                id.push_str("_alacritty");
                id
            },
        };

        Self { id, uri }
    }
}

/// Trait for determining if a reset should be performed.
pub trait ResetDiscriminant<T> {
    /// Value based on which equality for the reset will be determined.
    fn discriminant(&self) -> T;
}

impl<T: Copy> ResetDiscriminant<T> for T {
    fn discriminant(&self) -> T {
        *self
    }
}

impl ResetDiscriminant<Color> for Cell {
    fn discriminant(&self) -> Color {
        self.bg
    }
}

/// Dynamically allocated cell content.
///
/// This storage is reserved for cell attributes which are rarely set. This allows reducing the
/// allocation required ahead of time for every cell, with some additional overhead when the extra
/// storage is actually required.
#[derive(Default, Debug, Clone, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct CellExtra {
    #[cfg_attr(feature = "serde", serde(skip))]
    reflow_anchor: Option<std::num::NonZeroU32>,
    zerowidth: smallvec::SmallVec<[char; 2]>,
    underline_color: Option<Color>,
    hyperlink: Option<Hyperlink>,
}

impl CellExtra {
    /// Combining characters or grapheme-tail scalars from an immutable snapshot.
    #[inline]
    pub fn zerowidth(&self) -> &[char] {
        &self.zerowidth
    }

    /// Hyperlink, borrowed without incrementing its reference count.
    #[inline]
    pub fn hyperlink(&self) -> Option<&Hyperlink> {
        self.hyperlink.as_ref()
    }
}

/// Content and attributes of a single cell in the terminal grid.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Cell {
    pub c: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: Flags,
    pub extra: Option<Arc<CellExtra>>,
}

impl Default for Cell {
    #[inline]
    fn default() -> Cell {
        Cell {
            c: ' ',
            bg: Color::Named(NamedColor::Background),
            fg: Color::Named(NamedColor::Foreground),
            flags: Flags::empty(),
            extra: None,
        }
    }
}

impl Cell {
    pub(super) fn reflow_anchor(&self) -> Option<usize> {
        self.extra.as_ref()?.reflow_anchor.map(|id| id.get() as usize - 1)
    }

    pub(super) fn set_reflow_anchor(&mut self, anchor: Option<usize>) {
        let extra = self.extra.get_or_insert_with(Default::default);
        Arc::make_mut(extra).reflow_anchor =
            anchor.and_then(|id| std::num::NonZeroU32::new(id as u32 + 1));
        if **extra == CellExtra::default() {
            self.extra = None;
        }
    }

    pub(crate) fn mark_packed_wrapped(row: &mut grid::PackedRow<Self>, column: usize) -> bool {
        row.expanded.take();
        let offset = column * 4;
        let cell = &mut row.bytes[offset..offset + 4];
        let value = u32::from_le_bytes(cell.try_into().unwrap());
        cell.copy_from_slice(&(value | (u32::from(Flags::WRAPLINE.bits()) << 21)).to_le_bytes());
        true
    }

    pub(crate) fn write_packed_wide<F: Fn(char) -> bool>(
        row: &mut grid::PackedRow<Self>,
        column: usize,
        chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
        eligible: F,
        template: &Self,
    ) -> Option<usize> {
        let style = &row.template;
        if style.fg != template.fg || style.bg != template.bg || style.flags != template.flags {
            return None;
        }
        row.expanded.take();
        let mut count = 0;
        for pair in row.bytes[column * 4..].chunks_exact_mut(8) {
            let Some(&c) = chars.peek() else { break };
            if !eligible(c)
                || pair.chunks_exact(4).any(|cell| {
                    let value = u32::from_le_bytes(cell.try_into().unwrap());
                    value & (u32::from((Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER).bits()) << 21)
                        != 0
                })
            {
                break;
            }
            chars.next();
            pair[..4].copy_from_slice(
                &((c as u32) | (u32::from(Flags::WIDE_CHAR.bits()) << 21)).to_le_bytes(),
            );
            pair[4..].copy_from_slice(
                &((b' ' as u32) | (u32::from(Flags::WIDE_CHAR_SPACER.bits()) << 21)).to_le_bytes(),
            );
            count += 2;
        }
        (count != 0).then_some(count)
    }

    pub(crate) fn blank_packed_row(columns: usize, bg: Color) -> grid::PackedRow<Self> {
        let mut bytes = vec![0; columns * 4];
        for cell in bytes.chunks_exact_mut(4) {
            cell[0] = b' ';
        }
        grid::PackedRow {
            template: Cell { bg, ..Cell::default() },
            bytes,
            expanded: Default::default(),
            decode: expand_packed_cells,
        }
    }

    pub(crate) fn write_packed_ascii(
        row: &mut grid::PackedRow<Self>,
        column: usize,
        text: &[u8],
        fg: Color,
        bg: Color,
        flags: Flags,
    ) -> Option<usize> {
        let template = &row.template;
        if template.fg != fg || template.bg != bg || template.flags != flags {
            return None;
        }
        let start = column * 4;
        let cells = &mut row.bytes[start..start + text.len() * 4];
        let count = cells
            .chunks_exact(4)
            .position(|cell| {
                let value = u32::from_le_bytes(cell.try_into().unwrap());
                value & (u32::from((Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER).bits()) << 21) != 0
            })
            .unwrap_or(text.len());
        if count == 0 {
            return None;
        }
        row.expanded.take();
        for (cell, &byte) in cells.chunks_exact_mut(4).zip(&text[..count]) {
            cell.copy_from_slice(&u32::from(byte).to_le_bytes());
        }
        Some(count)
    }

    /// Additional scalars stored in this cell (combining marks or a grapheme tail).
    #[inline]
    pub fn zerowidth(&self) -> Option<&[char]> {
        self.extra.as_ref().map(|extra| extra.zerowidth.as_slice())
    }

    /// Write a new zerowidth character to this cell.
    #[inline]
    pub fn push_zerowidth(&mut self, character: char) {
        let extra = self.extra.get_or_insert_with(Default::default);
        Arc::make_mut(extra).zerowidth.push(character);
    }

    /// Remove all wide char data from a cell.
    #[inline(never)]
    pub fn clear_wide(&mut self) {
        self.flags.remove(Flags::WIDE_CHAR | Flags::GRAPHEME);
        if let Some(extra) = self.extra.as_mut() {
            if extra.underline_color.is_none() && extra.hyperlink.is_none() {
                self.extra = None;
            } else if let Some(extra) = Arc::get_mut(extra) {
                extra.zerowidth.clear();
            } else {
                // Preserve shared render snapshots without first cloning the
                // grapheme data we are about to discard.
                *extra = Arc::new(CellExtra {
                    reflow_anchor: None,
                    zerowidth: Default::default(),
                    underline_color: extra.underline_color,
                    hyperlink: extra.hyperlink.clone(),
                });
            }
        }
        self.c = ' ';
    }

    /// Set underline color on the cell.
    pub fn set_underline_color(&mut self, color: Option<Color>) {
        // If we reset color and we don't have zerowidth we should drop extra storage.
        if color.is_none()
            && self
                .extra
                .as_ref()
                .is_none_or(|extra| extra.zerowidth.is_empty() && extra.hyperlink.is_none())
        {
            self.extra = None;
        } else {
            let extra = self.extra.get_or_insert_with(Default::default);
            Arc::make_mut(extra).underline_color = color;
        }
    }

    /// Underline color stored in this cell.
    #[inline]
    pub fn underline_color(&self) -> Option<Color> {
        self.extra.as_ref()?.underline_color
    }

    /// Set hyperlink.
    pub fn set_hyperlink(&mut self, hyperlink: Option<Hyperlink>) {
        let should_drop = hyperlink.is_none()
            && self
                .extra
                .as_ref()
                .is_none_or(|extra| extra.zerowidth.is_empty() && extra.underline_color.is_none());

        if should_drop {
            self.extra = None;
        } else {
            let extra = self.extra.get_or_insert_with(Default::default);
            Arc::make_mut(extra).hyperlink = hyperlink;
        }
    }

    /// Hyperlink stored in this cell.
    #[inline]
    pub fn hyperlink(&self) -> Option<Hyperlink> {
        self.extra.as_ref()?.hyperlink.clone()
    }
}

impl GridCell for Cell {
    fn reset_packed(row: &mut grid::PackedRow<Self>, template: &Self) -> bool {
        row.expanded.take();
        row.template = Cell { bg: template.bg, ..Cell::default() };
        for cell in row.bytes.chunks_exact_mut(4) {
            cell.copy_from_slice(&(b' ' as u32).to_le_bytes());
        }
        true
    }

    fn pack_rows(rows: &[&[Self]]) -> Option<Vec<grid::PackedRow<Self>>> {
        let first = rows.first()?.first()?;
        if first.extra.is_some() {
            return None;
        }
        let flags = first.flags & !PACKED_POSITION_FLAGS;
        // Common text shares its colors/style while wide-cell and wrap flags
        // vary by position. Store its scalars and those flags in four bytes,
        // retaining the original Cell representation for other styles.
        if !rows.iter().flat_map(|row| row.iter()).all(|cell| {
            cell.fg == first.fg
                && cell.bg == first.bg
                && cell.flags & !PACKED_POSITION_FLAGS == flags
                && cell.extra.is_none()
        }) {
            return None;
        }
        Some(
            rows.iter()
                .map(|row| {
                    let mut bytes = Vec::with_capacity(row.len() * 4);
                    for cell in *row {
                        let position = u32::from((cell.flags & PACKED_POSITION_FLAGS).bits());
                        bytes.extend_from_slice(&((cell.c as u32) | position << 21).to_le_bytes());
                    }
                    grid::PackedRow {
                        template: Cell { c: '\0', flags, ..first.clone() },
                        bytes,
                        expanded: Default::default(),
                        decode: expand_packed_cells,
                    }
                })
                .collect(),
        )
    }

    fn compact_row(cells: &[Self]) -> Option<grid::CompactRow<Self>> {
        Self::compact_rows(&[cells]).and_then(|mut rows| rows.pop())
    }

    fn compact_rows(rows: &[&[Self]]) -> Option<Vec<grid::CompactRow<Self>>> {
        let count: usize = rows.iter().map(|row| row.len()).sum();
        // Blocks are small, bounded groups of rows. If an unusually large
        // caller exceeds the compact offset range, preserve its dense cells.
        if count < 16 || count > u32::MAX as usize {
            return None;
        }
        let mut templates: Vec<Cell> = Vec::new();
        let mut encoded = Vec::with_capacity(count * 6);
        for cell in rows.iter().flat_map(|row| row.iter()) {
            if cell.reflow_anchor().is_some() {
                return None;
            }
            let index = templates.iter().position(|template| {
                template.fg == cell.fg
                    && template.bg == cell.bg
                    && template.flags == cell.flags
                    && template.underline_color() == cell.underline_color()
                    && template.extra.as_ref().and_then(|extra| extra.hyperlink.as_ref())
                        == cell.extra.as_ref().and_then(|extra| extra.hyperlink.as_ref())
            });
            let index = match index {
                Some(index) => index,
                None => {
                    // Bound dictionary work for highly varied cells. Those rows
                    // remain resident; no data or attributes are discarded.
                    if templates.len() == 32 {
                        return None;
                    }
                    // Grapheme tails are content, not style. Keeping them in
                    // the dictionary retained thousands of tiny heap objects
                    // and prevented neighboring rows from sharing a block.
                    let extra = cell.extra.as_ref().and_then(|extra| {
                        (extra.underline_color.is_some() || extra.hyperlink.is_some()).then(|| {
                            Arc::new(CellExtra {
                                reflow_anchor: None,
                                zerowidth: Default::default(),
                                underline_color: extra.underline_color,
                                hyperlink: extra.hyperlink.clone(),
                            })
                        })
                    });
                    templates.push(Cell { c: '\0', extra, ..cell.clone() });
                    templates.len() - 1
                },
            };
            encoded.push(index as u8);
            encoded.extend_from_slice(&(cell.c as u32).to_le_bytes());
            // Zero means no extra object. Other short tags encode tail length
            // plus one, preserving even an explicitly empty CellExtra. Long
            // tails use a checked u32 length instead of truncating content.
            if let Some(extra) = &cell.extra {
                let len = extra.zerowidth.len();
                if len < 254 {
                    encoded.push((len + 1) as u8);
                } else {
                    encoded.push(255);
                    encoded.extend_from_slice(&u32::try_from(len).ok()?.to_le_bytes());
                }
                for &c in &extra.zerowidth {
                    encoded.extend_from_slice(&(c as u32).to_le_bytes());
                }
            } else {
                encoded.push(0);
            }
        }
        let bytes = lz4_flex::compress_prepend_size(&encoded);
        let size = bytes.len()
            + templates.len() * std::mem::size_of::<Cell>()
            + std::mem::size_of::<grid::CompactBlock<Cell>>()
            + rows.len() * std::mem::size_of::<grid::CompactRow<Cell>>();
        if size >= count * std::mem::size_of::<Cell>() / 2 {
            return None;
        }
        templates.shrink_to_fit();
        let block = Arc::new(grid::CompactBlock {
            templates,
            bytes,
            decode: expand_cells,
            clone_cells: |cells| cells.to_vec(),
            len: count,
            expanded: Default::default(),
        });
        let mut start = 0_u32;
        Some(
            rows.iter()
                .map(|row| {
                    let len = row.len() as u32;
                    let compact = grid::CompactRow { block: block.clone(), start, len };
                    start += len;
                    compact
                })
                .collect(),
        )
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.reflow_anchor().is_none()
            && (self.c == ' ' || self.c == '\t')
            && self.bg == Color::Named(NamedColor::Background)
            && self.fg == Color::Named(NamedColor::Foreground)
            && !self.flags.intersects(
                Flags::INVERSE
                    | Flags::ALL_UNDERLINES
                    | Flags::STRIKEOUT
                    | Flags::WRAPLINE
                    | Flags::WIDE_CHAR_SPACER
                    | Flags::LEADING_WIDE_CHAR_SPACER,
            )
            && self.extra.as_ref().map(|extra| extra.zerowidth.is_empty()) != Some(false)
    }

    #[inline]
    fn flags(&self) -> &Flags {
        &self.flags
    }

    #[inline]
    fn flags_mut(&mut self) -> &mut Flags {
        &mut self.flags
    }

    #[inline]
    fn reset(&mut self, template: &Self) {
        *self = Cell { bg: template.bg, ..Cell::default() };
    }
}

// These bits occupy at most bit 10. Shifting by 21 leaves a Unicode scalar's
// low 21 bits intact and fits exactly in a u32, including the leading spacer.
const PACKED_POSITION_FLAGS: Flags = Flags::from_bits_retain(
    Flags::WRAPLINE.bits()
        | Flags::WIDE_CHAR.bits()
        | Flags::WIDE_CHAR_SPACER.bits()
        | Flags::LEADING_WIDE_CHAR_SPACER.bits(),
);

fn expand_packed_cells(
    templates: &[Cell],
    bytes: &[u8],
    range: std::ops::Range<usize>,
) -> Vec<Cell> {
    bytes[range.start * 4..range.end * 4]
        .chunks_exact(4)
        .map(|chunk| {
            let value = u32::from_le_bytes(chunk.try_into().unwrap());
            let c = char::from_u32(value & 0x1f_ffff).unwrap();
            let position = Flags::from_bits_retain((value >> 21) as u16) & PACKED_POSITION_FLAGS;
            Cell { c, flags: templates[0].flags | position, ..templates[0].clone() }
        })
        .collect()
}

fn expand_cells(templates: &[Cell], bytes: &[u8], range: std::ops::Range<usize>) -> Vec<Cell> {
    // Only our encoder produces these bytes; they are never read from disk or
    // a process. Checked decoding keeps corruption from becoming memory unsafety.
    let encoded =
        lz4_flex::decompress_size_prepended(bytes).expect("invalid internal row encoding");
    let mut cells = Vec::with_capacity(range.len());
    let mut offset = 0;
    for position in 0..range.end {
        let header = &encoded[offset..offset + 6];
        offset += 6;
        let tag = header[5];
        let len = if tag == 255 {
            let len = u32::from_le_bytes(encoded[offset..offset + 4].try_into().unwrap());
            offset += 4;
            len as usize
        } else {
            tag.saturating_sub(1) as usize
        };
        let tail = &encoded[offset..offset + len * 4];
        offset += tail.len();
        if position < range.start {
            continue;
        }
        let c = char::from_u32(u32::from_le_bytes(header[1..5].try_into().unwrap())).unwrap();
        let mut cell = Cell { c, ..templates[header[0] as usize].clone() };
        if len != 0 {
            let mut extra = cell.extra.as_deref().cloned().unwrap_or_default();
            extra.zerowidth.extend(tail.chunks_exact(4).map(|scalar| {
                char::from_u32(u32::from_le_bytes(scalar.try_into().unwrap())).unwrap()
            }));
            cell.extra = Some(Arc::new(extra));
        } else if tag != 0 && cell.extra.is_none() {
            cell.extra = Some(Arc::default());
        }
        cells.push(cell);
    }
    cells
}

impl From<Color> for Cell {
    #[inline]
    fn from(color: Color) -> Self {
        Self { bg: color, ..Cell::default() }
    }
}

/// Get the length of occupied cells in a line.
pub trait LineLength {
    /// Calculate the occupied line length.
    fn line_length(&self) -> Column;
}

impl LineLength for grid::Row<Cell> {
    fn line_length(&self) -> Column {
        let mut length = Column(0);

        if self[Column(self.len() - 1)].flags.contains(Flags::WRAPLINE) {
            return Column(self.len());
        }

        for (index, cell) in self[..].iter().rev().enumerate() {
            if cell.c != ' '
                || cell.extra.as_ref().map(|extra| extra.zerowidth.is_empty()) == Some(false)
            {
                length = Column(self.len() - index);
                break;
            }
        }

        length
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::mem;

    use crate::grid::Row;
    use crate::index::Column;

    #[test]
    fn clearing_wide_cell_preserves_snapshot_and_attributes() {
        let mut cell =
            Cell { c: '❤', flags: Flags::WIDE_CHAR | Flags::GRAPHEME, ..Cell::default() };
        cell.push_zerowidth('\u{fe0f}');
        cell.set_underline_color(Some(Color::Indexed(2)));
        let link = Hyperlink::new(Some("test"), "https://example.com".into());
        cell.set_hyperlink(Some(link.clone()));
        let snapshot = cell.clone();
        cell.clear_wide();
        assert_eq!(cell.c, ' ');
        assert_eq!(cell.zerowidth(), Some([].as_slice()));
        assert_eq!(cell.underline_color(), Some(Color::Indexed(2)));
        assert_eq!(cell.hyperlink(), Some(link));
        assert_eq!(snapshot.c, '❤');
        assert_eq!(snapshot.zerowidth(), Some(['\u{fe0f}'].as_slice()));
        assert!(snapshot.flags.contains(Flags::WIDE_CHAR | Flags::GRAPHEME));
    }

    #[test]
    fn short_and_long_graphemes_preserve_shared_snapshots() {
        let mut cell = Cell::default();
        cell.push_zerowidth('\u{301}');
        let snapshot = cell.clone();
        for _ in 0..16 {
            cell.push_zerowidth('\u{308}');
        }
        assert_eq!(snapshot.zerowidth(), Some(['\u{301}'].as_slice()));
        assert_eq!(cell.zerowidth().unwrap().len(), 17);
        cell.clear_wide();
        assert!(cell.extra.is_none());
        assert_eq!(snapshot.zerowidth(), Some(['\u{301}'].as_slice()));
    }

    #[test]
    fn cell_size_is_below_cap() {
        // Expected cell size on 64-bit architectures.
        const EXPECTED_CELL_SIZE: usize = 24;

        // Ensure that cell size isn't growing by accident.
        assert!(mem::size_of::<Cell>() <= EXPECTED_CELL_SIZE);
    }

    #[test]
    fn line_length_works() {
        let mut row = Row::<Cell>::new(10);
        row[Column(5)].c = 'a';

        assert_eq!(row.line_length(), Column(6));
    }

    #[test]
    fn line_length_works_with_wrapline() {
        let mut row = Row::<Cell>::new(10);
        row[Column(9)].flags.insert(super::Flags::WRAPLINE);

        assert_eq!(row.line_length(), Column(10));
    }
}
