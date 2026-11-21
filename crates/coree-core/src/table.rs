// SPDX-License-Identifier: MIT OR Apache-2.0

//! Editable vector tables with cell merging, row/column insertion & deletion,
//! styling, and grid curve extraction.

use crate::error::{CoreError, CoreResult};
use crate::geometry::{BezierPath, Subpath};
use crate::math::{sanitize_f64, Point2D, Rect2D};
use crate::style::{Fill, OutlineStyle};
use crate::text::TextStyle;
use serde::{Deserialize, Serialize};

/// A single cell inside a [`VectorTable`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableCell {
    pub row: usize,
    pub col: usize,
    pub row_span: usize,
    pub col_span: usize,
    pub text: String,
    pub text_style: TextStyle,
    pub fill: Fill,
    pub padding_mm: f64,
    /// When covered by a merged span, points to the top-left anchor cell `(row, col)`.
    pub merged_into: Option<(usize, usize)>,
}

impl TableCell {
    /// Creates an unmerged default cell at `(row, col)`.
    #[must_use]
    pub fn new(row: usize, col: usize) -> Self {
        Self {
            row,
            col,
            row_span: 1,
            col_span: 1,
            text: String::new(),
            text_style: TextStyle {
                font_size_pt: 11.0,
                ..TextStyle::default()
            },
            fill: Fill::None,
            padding_mm: 2.0,
            merged_into: None,
        }
    }

    /// Returns `true` if this cell is hidden underneath another cell's merge span.
    #[must_use]
    pub const fn is_covered(&self) -> bool {
        self.merged_into.is_some()
    }
}

/// Interactive grid table object whose geometry and cells can be styled or converted to curves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorTable {
    pub rows: usize,
    pub cols: usize,
    pub col_widths_mm: Vec<f64>,
    pub row_heights_mm: Vec<f64>,
    pub cells: Vec<TableCell>,
    pub border_style: OutlineStyle,
}

impl VectorTable {
    /// Maximum supported rows or columns per table (guards against OOM/overflow).
    pub const MAX_DIM: usize = 200;

    /// Creates a `rows × cols` table with uniform cell size (clamped to `1..=200`).
    #[must_use]
    pub fn new(rows: usize, cols: usize, col_width_mm: f64, row_height_mm: f64) -> Self {
        let rows = rows.clamp(1, Self::MAX_DIM);
        let cols = cols.clamp(1, Self::MAX_DIM);
        let cw = sanitize_f64(col_width_mm, 25.0).max(1.0);
        let rh = sanitize_f64(row_height_mm, 10.0).max(1.0);
        let mut cells = Vec::with_capacity(rows * cols);
        for r in 0..rows {
            for c in 0..cols {
                cells.push(TableCell::new(r, c));
            }
        }
        Self {
            rows,
            cols,
            col_widths_mm: vec![cw; cols],
            row_heights_mm: vec![rh; rows],
            cells,
            border_style: OutlineStyle::default(),
        }
    }

    /// Flattened cell index for `(row, col)`.
    #[inline]
    fn cell_index(&self, row: usize, col: usize) -> Option<usize> {
        if row < self.rows && col < self.cols {
            let idx = row * self.cols + col;
            (idx < self.cells.len()).then_some(idx)
        } else {
            None
        }
    }

    /// Immutable reference to cell `(row, col)`.
    #[must_use]
    pub fn cell(&self, row: usize, col: usize) -> Option<&TableCell> {
        self.cell_index(row, col).map(|i| &self.cells[i])
    }

    /// Mutable reference to cell `(row, col)`.
    #[must_use]
    pub fn cell_mut(&mut self, row: usize, col: usize) -> Option<&mut TableCell> {
        self.cell_index(row, col).map(|i| &mut self.cells[i])
    }

    /// Local-space bounding rectangle of cell `(row, col)` including its merge span.
    #[must_use]
    pub fn cell_rect(&self, row: usize, col: usize) -> Option<Rect2D> {
        let cell = self.cell(row, col)?;
        if cell.is_covered() {
            return None;
        }
        let x0: f64 = self.col_widths_mm.iter().take(col).sum();
        let y0: f64 = self.row_heights_mm.iter().take(row).sum();
        let w: f64 = self
            .col_widths_mm
            .iter()
            .skip(col)
            .take(cell.col_span.max(1))
            .sum();
        let h: f64 = self
            .row_heights_mm
            .iter()
            .skip(row)
            .take(cell.row_span.max(1))
            .sum();
        Some(Rect2D::from_xywh(x0, y0, w, h))
    }

    /// Merges the rectangular cell range `[r0..=r1, c0..=c1]` into a single spanned cell.
    pub fn merge_cells(&mut self, r0: usize, c0: usize, r1: usize, c1: usize) -> CoreResult<()> {
        let (r_min, r_max) = (r0.min(r1), r0.max(r1));
        let (c_min, c_max) = (c0.min(c1), c0.max(c1));
        if r_max >= self.rows || c_max >= self.cols {
            return Err(CoreError::InvalidTableOp(
                "merge coordinates out of table bounds".into(),
            ));
        }
        if r_min == r_max && c_min == c_max {
            return Ok(());
        }
        // Reject partial overlaps with existing merged spans
        for r in r_min..=r_max {
            for c in c_min..=c_max {
                if let Some(cell) = self.cell(r, c) {
                    if let Some((ar, ac)) = cell.merged_into {
                        if ar < r_min || ar > r_max || ac < c_min || ac > c_max {
                            return Err(CoreError::InvalidTableOp(
                                "selection partially overlaps an existing merged cell".into(),
                            ));
                        }
                    }
                    if r + cell.row_span.max(1) - 1 > r_max || c + cell.col_span.max(1) - 1 > c_max
                    {
                        return Err(CoreError::InvalidTableOp(
                            "selection partially covers an existing merged span".into(),
                        ));
                    }
                }
            }
        }

        let mut combined_text = String::new();
        for r in r_min..=r_max {
            for c in c_min..=c_max {
                if let Some(cell) = self.cell(r, c) {
                    if !cell.text.is_empty() {
                        if !combined_text.is_empty() {
                            combined_text.push(' ');
                        }
                        combined_text.push_str(&cell.text);
                    }
                }
            }
        }

        for r in r_min..=r_max {
            for c in c_min..=c_max {
                if let Some(cell) = self.cell_mut(r, c) {
                    if r == r_min && c == c_min {
                        cell.row_span = r_max - r_min + 1;
                        cell.col_span = c_max - c_min + 1;
                        cell.merged_into = None;
                        cell.text = combined_text.clone();
                    } else {
                        cell.row_span = 1;
                        cell.col_span = 1;
                        cell.merged_into = Some((r_min, c_min));
                        cell.text.clear();
                    }
                }
            }
        }
        Ok(())
    }

    /// Splits a merged cell at `(row, col)` back into individual 1×1 cells.
    pub fn unmerge_cell(&mut self, row: usize, col: usize) -> CoreResult<()> {
        let (anchor_r, anchor_c, rs, cs) = {
            let cell = self
                .cell(row, col)
                .ok_or_else(|| CoreError::InvalidTableOp("cell out of bounds".into()))?;
            let (ar, ac) = cell.merged_into.unwrap_or((row, col));
            let anchor = self
                .cell(ar, ac)
                .ok_or_else(|| CoreError::InvalidTableOp("anchor cell missing".into()))?;
            (ar, ac, anchor.row_span, anchor.col_span)
        };
        for r in anchor_r..(anchor_r + rs).min(self.rows) {
            for c in anchor_c..(anchor_c + cs).min(self.cols) {
                if let Some(cell) = self.cell_mut(r, c) {
                    cell.row_span = 1;
                    cell.col_span = 1;
                    cell.merged_into = None;
                }
            }
        }
        Ok(())
    }

    /// Inserts a new row at index `at` (`0..=self.rows`).
    pub fn insert_row(&mut self, at: usize, height_mm: f64) -> CoreResult<()> {
        if self.rows >= Self::MAX_DIM {
            return Err(CoreError::InvalidTableOp("table row limit reached".into()));
        }
        if at > self.rows {
            return Err(CoreError::InvalidTableOp(
                "insert_row index out of bounds".into(),
            ));
        }
        let h = sanitize_f64(height_mm, 10.0).max(1.0);
        self.row_heights_mm.insert(at, h);
        let new_rows = self.rows + 1;

        let mut new_cells = Vec::with_capacity(new_rows * self.cols);
        for r in 0..new_rows {
            for c in 0..self.cols {
                if r == at {
                    new_cells.push(TableCell::new(r, c));
                } else {
                    let old_r = if r < at { r } else { r - 1 };
                    let mut cell = self
                        .cells
                        .get(old_r * self.cols + c)
                        .cloned()
                        .unwrap_or_else(|| TableCell::new(r, c));
                    cell.row = r;
                    if let Some((ar, ac)) = cell.merged_into {
                        let new_ar = if ar >= at { ar + 1 } else { ar };
                        cell.merged_into = Some((new_ar, ac));
                    }
                    // If a merge above `at` spanned across `at`, unmerge cleanly to keep invariants valid
                    if old_r < at && old_r + cell.row_span > at {
                        cell.row_span = at - old_r;
                    }
                    new_cells.push(cell);
                }
            }
        }
        self.rows = new_rows;
        self.cells = new_cells;
        self.repair_merge_invariants();
        Ok(())
    }

    /// Deletes row `at` (fails if this is the only remaining row).
    pub fn delete_row(&mut self, at: usize) -> CoreResult<()> {
        if self.rows <= 1 {
            return Err(CoreError::InvalidTableOp(
                "cannot delete the last remaining row".into(),
            ));
        }
        if at >= self.rows {
            return Err(CoreError::InvalidTableOp(
                "delete_row index out of bounds".into(),
            ));
        }
        self.row_heights_mm.remove(at);
        let mut new_cells = Vec::with_capacity((self.rows - 1) * self.cols);
        for r in 0..self.rows {
            if r == at {
                continue;
            }
            let new_r = if r < at { r } else { r - 1 };
            for c in 0..self.cols {
                let mut cell = self
                    .cells
                    .get(r * self.cols + c)
                    .cloned()
                    .unwrap_or_else(|| TableCell::new(new_r, c));
                cell.row = new_r;
                if let Some((ar, ac)) = cell.merged_into {
                    if ar == at {
                        cell.merged_into = None;
                    } else {
                        cell.merged_into = Some((if ar > at { ar - 1 } else { ar }, ac));
                    }
                }
                if r < at && r + cell.row_span > at {
                    cell.row_span = cell.row_span.saturating_sub(1).max(1);
                }
                new_cells.push(cell);
            }
        }
        self.rows -= 1;
        self.cells = new_cells;
        self.repair_merge_invariants();
        Ok(())
    }

    /// Inserts a new column at index `at` (`0..=self.cols`).
    pub fn insert_col(&mut self, at: usize, width_mm: f64) -> CoreResult<()> {
        if self.cols >= Self::MAX_DIM {
            return Err(CoreError::InvalidTableOp(
                "table column limit reached".into(),
            ));
        }
        if at > self.cols {
            return Err(CoreError::InvalidTableOp(
                "insert_col index out of bounds".into(),
            ));
        }
        let w = sanitize_f64(width_mm, 25.0).max(1.0);
        self.col_widths_mm.insert(at, w);
        let new_cols = self.cols + 1;
        let mut new_cells = Vec::with_capacity(self.rows * new_cols);
        for r in 0..self.rows {
            for c in 0..new_cols {
                if c == at {
                    new_cells.push(TableCell::new(r, c));
                } else {
                    let old_c = if c < at { c } else { c - 1 };
                    let mut cell = self
                        .cells
                        .get(r * self.cols + old_c)
                        .cloned()
                        .unwrap_or_else(|| TableCell::new(r, c));
                    cell.col = c;
                    if let Some((ar, ac)) = cell.merged_into {
                        let new_ac = if ac >= at { ac + 1 } else { ac };
                        cell.merged_into = Some((ar, new_ac));
                    }
                    if old_c < at && old_c + cell.col_span > at {
                        cell.col_span = at - old_c;
                    }
                    new_cells.push(cell);
                }
            }
        }
        self.cols = new_cols;
        self.cells = new_cells;
        self.repair_merge_invariants();
        Ok(())
    }

    /// Deletes column `at` (fails if this is the only remaining column).
    pub fn delete_col(&mut self, at: usize) -> CoreResult<()> {
        if self.cols <= 1 {
            return Err(CoreError::InvalidTableOp(
                "cannot delete the last remaining column".into(),
            ));
        }
        if at >= self.cols {
            return Err(CoreError::InvalidTableOp(
                "delete_col index out of bounds".into(),
            ));
        }
        self.col_widths_mm.remove(at);
        let new_cols = self.cols - 1;
        let mut new_cells = Vec::with_capacity(self.rows * new_cols);
        for r in 0..self.rows {
            for c in 0..self.cols {
                if c == at {
                    continue;
                }
                let new_c = if c < at { c } else { c - 1 };
                let mut cell = self
                    .cells
                    .get(r * self.cols + c)
                    .cloned()
                    .unwrap_or_else(|| TableCell::new(r, new_c));
                cell.col = new_c;
                if let Some((ar, ac)) = cell.merged_into {
                    if ac == at {
                        cell.merged_into = None;
                    } else {
                        cell.merged_into = Some((ar, if ac > at { ac - 1 } else { ac }));
                    }
                }
                if c < at && c + cell.col_span > at {
                    cell.col_span = cell.col_span.saturating_sub(1).max(1);
                }
                new_cells.push(cell);
            }
        }
        self.cols = new_cols;
        self.cells = new_cells;
        self.repair_merge_invariants();
        Ok(())
    }

    fn repair_merge_invariants(&mut self) {
        let rows = self.rows;
        let cols = self.cols;
        // Pass 1: normalize spans and strip out-of-bounds/self anchors
        for (idx, cell) in self.cells.iter_mut().enumerate() {
            cell.row = idx / cols.max(1);
            cell.col = idx % cols.max(1);
            cell.row_span = cell
                .row_span
                .max(1)
                .min(rows.saturating_sub(cell.row).max(1));
            cell.col_span = cell
                .col_span
                .max(1)
                .min(cols.saturating_sub(cell.col).max(1));
            if let Some((ar, ac)) = cell.merged_into {
                if ar >= rows || ac >= cols || (ar == cell.row && ac == cell.col) {
                    cell.merged_into = None;
                }
            }
        }
        // Pass 2: clear `merged_into` for any cell outside its anchor's span or pointing at a covered cell
        let snapshot: Vec<(usize, usize, bool)> = self
            .cells
            .iter()
            .map(|c| (c.row_span, c.col_span, c.merged_into.is_some()))
            .collect();
        for cell in &mut self.cells {
            if let Some((ar, ac)) = cell.merged_into {
                let anchor_idx = ar * cols + ac;
                let valid = snapshot
                    .get(anchor_idx)
                    .is_some_and(|&(rs, cs, anchor_covered)| {
                        !anchor_covered
                            && cell.row >= ar
                            && cell.row < ar.saturating_add(rs)
                            && cell.col >= ac
                            && cell.col < ac.saturating_add(cs)
                    });
                if !valid {
                    cell.merged_into = None;
                }
            }
        }
    }

    /// Total bounding box of the table in local millimeter coordinates.
    #[must_use]
    pub fn to_bounding_box(&self) -> Rect2D {
        let total_w: f64 = self
            .col_widths_mm
            .iter()
            .map(|&w| sanitize_f64(w, 0.0).max(0.0))
            .sum();
        let total_h: f64 = self
            .row_heights_mm
            .iter()
            .map(|&h| sanitize_f64(h, 0.0).max(0.0))
            .sum();
        Rect2D::from_xywh(0.0, 0.0, total_w, total_h)
    }

    /// Converts visible cell outlines into a [`BezierPath`] (used by Convert to Curves).
    #[must_use]
    pub fn to_grid_paths(&self) -> BezierPath {
        let mut subpaths = Vec::new();
        for r in 0..self.rows {
            for c in 0..self.cols {
                if let Some(rect) = self.cell_rect(r, c) {
                    subpaths.push(Subpath::from_points(
                        [
                            Point2D::new(rect.min_x, rect.min_y),
                            Point2D::new(rect.max_x, rect.min_y),
                            Point2D::new(rect.max_x, rect.max_y),
                            Point2D::new(rect.min_x, rect.max_y),
                        ],
                        true,
                    ));
                }
            }
        }
        BezierPath { subpaths }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_merge_insert_delete_and_bounds() {
        let mut t = VectorTable::new(3, 4, 20.0, 10.0);
        assert_eq!(t.to_bounding_box(), Rect2D::from_xywh(0.0, 0.0, 80.0, 30.0));

        t.cell_mut(0, 0).unwrap().text = "Header".into();
        t.merge_cells(0, 0, 0, 1).unwrap();
        assert_eq!(t.cell(0, 0).unwrap().col_span, 2);
        assert!(t.cell(0, 1).unwrap().is_covered());
        assert_eq!(
            t.cell_rect(0, 0),
            Some(Rect2D::from_xywh(0.0, 0.0, 40.0, 10.0))
        );
        assert_eq!(t.cell_rect(0, 1), None);

        t.insert_row(1, 15.0).unwrap();
        assert_eq!(t.rows, 4);
        assert_eq!(t.to_bounding_box().height(), 45.0);

        t.insert_col(2, 30.0).unwrap();
        assert_eq!(t.cols, 5);
        assert_eq!(t.to_bounding_box().width(), 110.0);

        t.delete_row(3).unwrap();
        t.delete_col(4).unwrap();
        assert_eq!(t.rows, 3);
        assert_eq!(t.cols, 4);

        t.unmerge_cell(0, 0).unwrap();
        assert!(!t.cell(0, 1).unwrap().is_covered());
        assert_eq!(t.to_grid_paths().subpaths.len(), 12);
    }
}
