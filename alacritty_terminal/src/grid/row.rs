//! Defines the Row type which makes up lines in the grid.

use std::cmp::{max, min};
use std::ops::{Index, IndexMut, Range, RangeFrom, RangeFull, RangeTo, RangeToInclusive};
use std::{ptr, slice};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use super::compact::Cells;
use crate::grid::GridCell;
use crate::index::Column;
use crate::term::cell::ResetDiscriminant;

/// A row in the grid.
#[derive(Default, Clone, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Row<T> {
    inner: Cells<T>,

    /// Maximum number of occupied entries.
    ///
    /// This is the upper bound on the number of elements in the row, which have been modified
    /// since the last reset. All cells after this point are guaranteed to be equal.
    pub(crate) occ: usize,
}

impl<T: PartialEq> PartialEq for Row<T> {
    fn eq(&self, other: &Self) -> bool {
        self.inner == other.inner
    }
}

impl<T: Default> Row<T> {
    /// Create a new terminal row.
    ///
    /// Ideally the `template` should be `Copy` in all performance sensitive scenarios.
    pub fn new(columns: usize) -> Row<T> {
        debug_assert!(columns >= 1);

        let mut inner: Vec<T> = Vec::with_capacity(columns);

        // This is a slightly optimized version of `std::vec::Vec::resize`.
        unsafe {
            let mut ptr = inner.as_mut_ptr();

            for _ in 1..columns {
                ptr::write(ptr, T::default());
                ptr = ptr.offset(1);
            }
            ptr::write(ptr, T::default());

            inner.set_len(columns);
        }

        Row { inner: Cells::Dense(inner), occ: 0 }
    }

    /// Increase the number of columns in the row.
    #[inline]
    pub fn grow(&mut self, columns: usize) {
        if self.inner.len() >= columns {
            return;
        }

        // New columns use T::default(), which can differ from the untouched
        // suffix of a cleared row (for example, its background color). Keep
        // occ's uniform-suffix guarantee across this new style boundary.
        self.occ = self.occ.max(self.inner.len());
        self.inner.make_mut_vec().resize_with(columns, T::default);
    }

    /// Reduce the number of columns in the row.
    ///
    /// This will return all non-empty cells that were removed.
    pub fn shrink(&mut self, columns: usize) -> Option<Vec<T>>
    where
        T: GridCell,
    {
        if self.inner.len() <= columns {
            return None;
        }

        // Split off cells for a new row.
        let mut new_row = self.inner.make_mut_vec().split_off(columns);
        let index = new_row.iter().rposition(|c| !c.is_empty()).map_or(0, |i| i + 1);
        new_row.truncate(index);

        self.occ = min(self.occ, columns);

        if new_row.is_empty() { None } else { Some(new_row) }
    }

    /// Reset all cells in the row to the `template` cell.
    #[inline]
    pub fn reset<D>(&mut self, template: &T)
    where
        T: ResetDiscriminant<D> + GridCell,
        D: PartialEq,
    {
        if let Cells::Packed(row) = &mut self.inner {
            if T::reset_packed(row, template) {
                self.occ = 0;
                return;
            }
        }
        if !self.is_dense() {
            // Recycled history is discarded, so do not decode it first.
            let len = self.inner.len();
            *self = Self::new(len);
            self.occ = len;
        }
        debug_assert!(!self.inner.is_empty());

        // Mark all cells as dirty if template cell changed.
        let len = self.inner.len();
        if self.inner[len - 1].discriminant() != template.discriminant() {
            self.occ = len;
        }

        // Reset every dirty cell in the row.
        for item in &mut self.inner[0..self.occ] {
            item.reset(template);
        }

        self.occ = 0;
    }
}

impl Row<crate::term::cell::Cell> {
    pub(crate) fn mark_wrapped(&mut self, column: Column) {
        use crate::term::cell::{Cell, Flags};
        if let Cells::Packed(row) = &mut self.inner {
            if Cell::mark_packed_wrapped(row, column.0) {
                self.occ = self.occ.max(column.0 + 1);
                return;
            }
        }
        self[column].flags.insert(Flags::WRAPLINE);
    }

    pub(crate) fn try_write_wide<F: Fn(char) -> bool>(
        &mut self,
        column: Column,
        chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
        eligible: F,
        template: &crate::term::cell::Cell,
    ) -> Option<usize> {
        use crate::term::cell::Cell;
        if self.is_dense() && self.occ == 0 && chars.size_hint().0 >= 8 {
            let first = &self.inner[0];
            if first.c == ' '
                && first.extra.is_none()
                && first.fg == template.fg
                && first.fg
                    == crate::vte::ansi::Color::Named(crate::vte::ansi::NamedColor::Foreground)
                && first.bg == template.bg
                && first.flags == template.flags
                && template.flags.is_empty()
            {
                self.set_packed(Cell::blank_packed_row(self.len(), template.bg));
            }
        }
        let Cells::Packed(row) = &mut self.inner else {
            return None;
        };
        let count = Cell::write_packed_wide(row, column.0, chars, eligible, template)?;
        self.occ = self.occ.max(column.0 + count);
        Some(count)
    }

    /// Plain ASCII can stay in the compact scalar representation even while
    /// being written. Complex edits transparently use the normal dense path.
    pub(crate) fn try_write_ascii(
        &mut self,
        column: Column,
        text: &[u8],
        fg: crate::vte::ansi::Color,
        bg: crate::vte::ansi::Color,
        flags: crate::term::cell::Flags,
    ) -> Option<usize> {
        use crate::term::cell::{Cell, Flags};
        use crate::vte::ansi::{Color, NamedColor};

        if self.is_dense() && self.occ == 0 && text.len() >= 16 {
            // All cells after occ have the same value. Only convert blank rows
            // whose untouched cells match the style we are about to write.
            let first = &self.inner[0];
            if first.c == ' '
                && first.extra.is_none()
                && first.flags == Flags::empty()
                && flags == Flags::empty()
                && first.fg == fg
                && fg == Color::Named(NamedColor::Foreground)
                && first.bg == bg
            {
                self.set_packed(Cell::blank_packed_row(self.len(), bg));
            }
        }
        let Cells::Packed(row) = &mut self.inner else {
            return None;
        };
        let count = Cell::write_packed_ascii(row, column.0, text, fg, bg, flags)?;
        self.occ = self.occ.max(column.0 + count);
        Some(count)
    }
}

#[allow(clippy::len_without_is_empty)]
impl<T> Row<T> {
    #[inline]
    pub(crate) fn is_packed(&self) -> bool {
        matches!(self.inner, Cells::Packed(_))
    }

    pub(super) fn is_dense(&self) -> bool {
        matches!(self.inner, Cells::Dense(_))
    }

    pub(super) fn set_packed(&mut self, row: super::PackedRow<T>) {
        self.inner = Cells::Packed(Box::new(row));
    }

    pub(super) fn is_cold_compact(&self) -> bool {
        matches!(&self.inner, Cells::Compact(row) if row.block.expanded.get().is_none())
    }

    pub(super) fn set_compact(&mut self, row: super::CompactRow<T>) {
        self.inner = Cells::Compact(row);
    }

    pub(super) fn compact(&mut self)
    where
        T: GridCell,
    {
        self.inner.compact();
    }

    #[inline]
    pub fn from_vec(vec: Vec<T>, occ: usize) -> Row<T> {
        Row { inner: Cells::Dense(vec), occ }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    #[inline]
    pub fn last(&self) -> Option<&T> {
        self.inner.last()
    }

    #[inline]
    pub fn last_mut(&mut self) -> Option<&mut T> {
        self.occ = self.inner.len();
        self.inner.last_mut()
    }

    #[inline]
    pub fn append(&mut self, vec: &mut Vec<T>)
    where
        T: GridCell,
    {
        self.occ += vec.len();
        self.inner.make_mut_vec().append(vec);
    }

    #[inline]
    pub fn append_front(&mut self, mut vec: Vec<T>) {
        self.occ += vec.len();

        vec.append(self.inner.make_mut_vec());
        self.inner = Cells::Dense(vec);
    }

    /// Check if all cells in the row are empty.
    #[inline]
    pub fn is_clear(&self) -> bool
    where
        T: GridCell,
    {
        self.inner.iter().all(GridCell::is_empty)
    }

    #[inline]
    pub fn front_split_off(&mut self, at: usize) -> Vec<T> {
        self.occ = self.occ.saturating_sub(at);

        let mut split = self.inner.make_mut_vec().split_off(at);
        std::mem::swap(&mut split, self.inner.make_mut_vec());
        split
    }
}

impl<'a, T> IntoIterator for &'a Row<T> {
    type IntoIter = slice::Iter<'a, T>;
    type Item = &'a T;

    #[inline]
    fn into_iter(self) -> slice::Iter<'a, T> {
        self.inner.iter()
    }
}

impl<'a, T> IntoIterator for &'a mut Row<T> {
    type IntoIter = slice::IterMut<'a, T>;
    type Item = &'a mut T;

    #[inline]
    fn into_iter(self) -> slice::IterMut<'a, T> {
        self.occ = self.len();
        self.inner.iter_mut()
    }
}

impl<T> Index<Column> for Row<T> {
    type Output = T;

    #[inline]
    fn index(&self, index: Column) -> &T {
        &self.inner[index.0]
    }
}

impl<T> IndexMut<Column> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: Column) -> &mut T {
        self.occ = max(self.occ, *index + 1);
        &mut self.inner[index.0]
    }
}

impl<T> Index<Range<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: Range<Column>) -> &[T] {
        &self.inner[(index.start.0)..(index.end.0)]
    }
}

impl<T> IndexMut<Range<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: Range<Column>) -> &mut [T] {
        self.occ = max(self.occ, *index.end);
        &mut self.inner[(index.start.0)..(index.end.0)]
    }
}

impl<T> Index<RangeTo<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: RangeTo<Column>) -> &[T] {
        &self.inner[..(index.end.0)]
    }
}

impl<T> IndexMut<RangeTo<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: RangeTo<Column>) -> &mut [T] {
        self.occ = max(self.occ, *index.end);
        &mut self.inner[..(index.end.0)]
    }
}

impl<T> Index<RangeFrom<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: RangeFrom<Column>) -> &[T] {
        &self.inner[(index.start.0)..]
    }
}

impl<T> IndexMut<RangeFrom<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: RangeFrom<Column>) -> &mut [T] {
        self.occ = self.len();
        &mut self.inner[(index.start.0)..]
    }
}

impl<T> Index<RangeFull> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, _: RangeFull) -> &[T] {
        &self.inner[..]
    }
}

impl<T> IndexMut<RangeFull> for Row<T> {
    #[inline]
    fn index_mut(&mut self, _: RangeFull) -> &mut [T] {
        self.occ = self.len();
        &mut self.inner[..]
    }
}

impl<T> Index<RangeToInclusive<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: RangeToInclusive<Column>) -> &[T] {
        &self.inner[..=(index.end.0)]
    }
}

impl<T> IndexMut<RangeToInclusive<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: RangeToInclusive<Column>) -> &mut [T] {
        self.occ = max(self.occ, *index.end + 1);
        &mut self.inner[..=(index.end.0)]
    }
}
