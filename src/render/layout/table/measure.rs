//! Table measurement phase — cell layout and border resolution.

use crate::render::dimension::Pt;

use crate::render::layout::cell::{layout_cell, CellLayout};

use super::borders::{
    border_width, resolve_border_conflict, resolve_cell_effective_borders, CellBorders,
};
use super::grid::{cell_index_at_grid_col, expand_rows_for_vmerge, is_vmerge_continue};
use super::types::{
    CellLayoutEntry, MeasuredRow, MeasuredTable, RowHeightRule, TableBorderConfig, TableBorderLine,
    TableRowInput, VerticalMergeState,
};

/// Measure all table rows: resolve borders, lay out cell content, compute heights.
/// This is the shared measurement phase used by both `layout_table` (monolithic)
/// and `layout_table_paginated` (page-splitting).
///
/// §17.4.38: `suppress_first_row_top` — when `true`, the top border of the first
/// row is suppressed. Used for adjacent table border collapse: consecutive tables
/// with the same style are treated as a single merged table, so the second table's
/// top border would duplicate the first table's bottom border.
pub(super) fn measure_table_rows(
    rows: &[TableRowInput],
    col_widths: &[Pt],
    default_line_height: Pt,
    borders: Option<&TableBorderConfig>,
    measure_text: crate::render::layout::paragraph::MeasureTextFn<'_>,
    suppress_first_row_top: bool,
) -> MeasuredTable {
    let table_width: Pt = col_widths.iter().copied().sum();
    let num_rows = rows.len();
    let mut row_heights = Vec::with_capacity(num_rows);

    // Pass 2a: resolve borders for every cell.
    let mut resolved_borders: Vec<Vec<CellBorders>> = Vec::new();
    {
        let mut grid_indices: Vec<Vec<usize>> = Vec::new();
        for (row_idx, row) in rows.iter().enumerate() {
            let mut row_borders = Vec::new();
            let mut row_grid = Vec::new();
            // §17.4.17: gridBefore — the row's first cell starts at grid_col
            // `grid_before`, leaving the leftmost columns empty.
            let mut grid_idx = row.grid_before as usize;
            // §17.4.61: a row may carry per-row border overrides
            // (`<w:tblPrEx><w:tblBorders/></w:tblPrEx>`). When set,
            // it's the *fully merged* effective table borders for this
            // row — the build layer already overlaid the override on
            // the table's own borders so the model-layer
            // "explicitly none" vs "not specified" distinction is
            // preserved during conversion. Use it verbatim; otherwise
            // fall back to the table-wide config.
            let row_table_borders = row.border_overrides.as_ref().or(borders);
            for cell_input in row.cells.iter() {
                let span = cell_input.grid_span.max(1) as usize;
                let (mut b_top, mut b_bottom, b_left, b_right) = resolve_cell_effective_borders(
                    cell_input,
                    row_table_borders,
                    row_idx,
                    grid_idx,
                    span,
                    num_rows,
                    col_widths.len(),
                );
                if cell_input.vertical_merge == Some(VerticalMergeState::Continue) {
                    b_top = None;
                }
                if row_idx + 1 < num_rows && is_vmerge_continue(&rows[row_idx + 1], grid_idx) {
                    b_bottom = None;
                }
                row_borders.push(CellBorders {
                    top: b_top,
                    bottom: b_bottom,
                    left: b_left,
                    right: b_right,
                });
                row_grid.push(grid_idx);
                grid_idx += cell_input.grid_span.max(1) as usize;
            }
            resolved_borders.push(row_borders);
            grid_indices.push(row_grid);
        }

        // §17.4.43: conflict resolution at vertical shared edges (a cell's
        // right vs. its right neighbour's left). Drawn once on the left cell.
        for row_idx in 0..num_rows {
            let num_cells = rows[row_idx].cells.len();
            for cell_ci in 0..num_cells.saturating_sub(1) {
                let right = resolved_borders[row_idx][cell_ci].right;
                let left = resolved_borders[row_idx][cell_ci + 1].left;
                let winner = resolve_border_conflict(right, left);
                resolved_borders[row_idx][cell_ci].right = winner;
                resolved_borders[row_idx][cell_ci + 1].left = None;
            }
        }

        // §17.4.43: conflict resolution at horizontal shared edges (row R's
        // bottom vs. row R+1's top). Resolved *per grid column* because a
        // `gridSpan` cell in one row can face several cells in the other:
        //   • wide upper cell over several lower cells — resolving only the
        //     first lower cell (and nulling the rest) drops their borders;
        //   • wide lower cell under several upper cells — a nil spacer among
        //     them must not punch a gap through the lower cell's border.
        //
        // The whole edge is then drawn from *one* side (all upper bottoms, or
        // all lower tops). This matters visually: an upper-row bottom sits in
        // the inter-row gap while a lower-row top sits just below it, so
        // splitting a single line between the two sides would offset segments
        // by the border width. A cell paints one border across its width, so
        // a side can own the edge only if each of its cells spans a run of
        // columns whose resolved border is uniform; upper is preferred (it
        // keeps the aligned-grid path and page-split top restoration valid).
        let ncols = col_widths.len();
        for upper in 0..num_rows.saturating_sub(1) {
            let lower = upper + 1;

            // Per-column resolved border for this inter-row edge.
            let resolved: Vec<Option<TableBorderLine>> = (0..ncols)
                .map(|gc| {
                    let ub = cell_index_at_grid_col(&rows[upper], gc)
                        .and_then(|ci| resolved_borders[upper][ci].bottom);
                    let lt = cell_index_at_grid_col(&rows[lower], gc)
                        .and_then(|ci| resolved_borders[lower][ci].top);
                    resolve_border_conflict(ub, lt)
                })
                .collect();

            // A row can paint the whole edge iff (a) it has a cell over every
            // column that carries a border — a row whose `gridSpan` leaves a
            // bordered column uncovered (its gridAfter gap) can't draw that
            // column, so the other row must — and (b) each of its cells spans
            // a uniform run of resolved columns (a cell paints one border
            // across its width). Without (a), a partly-covered cell would draw
            // its own top *and* the covering row its bottom → a doubled line.
            let can_own = |row_idx: usize| -> bool {
                let covers_bordered_cols = (0..ncols).all(|gc| {
                    resolved[gc].is_none() || cell_index_at_grid_col(&rows[row_idx], gc).is_some()
                });
                covers_bordered_cols
                    && grid_indices[row_idx]
                        .iter()
                        .enumerate()
                        .all(|(ci, &start)| {
                            let span = rows[row_idx].cells[ci].grid_span.max(1) as usize;
                            let end = (start + span).min(ncols);
                            start >= end || (start..end).all(|gc| resolved[gc] == resolved[start])
                        })
            };

            if !can_own(upper) && can_own(lower) {
                // Wide upper cell can't paint the mixed edge; draw it entirely
                // from the finer lower row so the line stays at one y (e.g. a
                // label cell right of a nil spacer under a gridSpan header).
                for (ci, &start) in grid_indices[lower].iter().enumerate() {
                    let span = rows[lower].cells[ci].grid_span.max(1) as usize;
                    let end = (start + span).min(ncols);
                    if start < end {
                        resolved_borders[lower][ci].top = resolved[start];
                    }
                }
                for b in resolved_borders[upper].iter_mut() {
                    b.bottom = None;
                }
            } else {
                // Upper row owns the edge: each upper cell paints its uniform
                // run (a nil spacer above a gridSpan cell resolves to that
                // cell's inherited border, so no gap), and lower tops it
                // covers are cleared. Columns an upper cell can't paint
                // uniformly (only reachable in the both-non-uniform fallback)
                // fall through to the lower cell.
                let mut covered = vec![false; ncols];
                for (ci, &start) in grid_indices[upper].iter().enumerate() {
                    let span = rows[upper].cells[ci].grid_span.max(1) as usize;
                    let end = (start + span).min(ncols);
                    if start >= end {
                        continue;
                    }
                    if (start..end).all(|gc| resolved[gc] == resolved[start]) {
                        resolved_borders[upper][ci].bottom = resolved[start];
                        for c in covered.iter_mut().take(end).skip(start) {
                            *c = true;
                        }
                    } else {
                        resolved_borders[upper][ci].bottom = None;
                    }
                }
                for (ci, &start) in grid_indices[lower].iter().enumerate() {
                    let span = rows[lower].cells[ci].grid_span.max(1) as usize;
                    let end = (start + span).min(ncols);
                    if start >= end {
                        continue;
                    }
                    if (start..end).any(|gc| covered[gc]) {
                        // Any column already painted from above → defer the
                        // whole cell so a partly-covered span can't double up.
                        resolved_borders[lower][ci].top = None;
                    } else if (start..end).all(|gc| resolved[gc] == resolved[start]) {
                        resolved_borders[lower][ci].top = resolved[start];
                    } else {
                        resolved_borders[lower][ci].top = None;
                    }
                }
            }
        }

        // §17.4.38: suppress first-row top borders for adjacent table collapse.
        if suppress_first_row_top && !resolved_borders.is_empty() {
            for b in &mut resolved_borders[0] {
                b.top = None;
            }
        }
    }

    // Pass 2b: lay out each cell.
    let mut row_cell_layouts: Vec<Vec<CellLayoutEntry>> = Vec::new();

    for (row_idx, row) in rows.iter().enumerate() {
        let mut entries = Vec::new();
        let mut max_height = Pt::ZERO;
        // §17.4.17: gridBefore — first cell offset.
        let mut grid_idx = row.grid_before as usize;

        for (cell_ci, cell) in row.cells.iter().enumerate() {
            let span = cell.grid_span.max(1) as usize;
            // Defensive clamp: malformed DOCX where gridBefore + spans + gridAfter
            // exceed the grid would otherwise panic in the slice index below.
            let grid_end = (grid_idx + span).min(col_widths.len());
            let cell_w: Pt = col_widths[grid_idx..grid_end].iter().copied().sum();
            let cell_x: Pt = col_widths[..grid_idx.min(col_widths.len())]
                .iter()
                .copied()
                .sum();

            let b = &resolved_borders[row_idx][cell_ci];
            let extra_left = (border_width(b.left) - cell.margins.left).max(Pt::ZERO);
            let extra_right = (border_width(b.right) - cell.margins.right).max(Pt::ZERO);
            let layout_w = (cell_w - extra_left - extra_right).max(Pt::ZERO);

            let is_continue = cell.vertical_merge == Some(VerticalMergeState::Continue);
            let layout = if is_continue {
                CellLayout {
                    commands: Vec::new(),
                    content_height: Pt::ZERO,
                }
            } else {
                layout_cell(
                    &cell.blocks,
                    layout_w,
                    &cell.margins,
                    default_line_height,
                    measure_text,
                )
            };

            if cell.vertical_merge.is_none() {
                max_height = max_height.max(layout.content_height + cell.margins.vertical());
            }

            entries.push(CellLayoutEntry {
                layout,
                cell_x,
                cell_w,
                grid_col: grid_idx,
            });
            grid_idx += span;
        }

        match row.height_rule {
            Some(RowHeightRule::AtLeast(min_h)) => max_height = max_height.max(min_h),
            Some(RowHeightRule::Exact(h)) => max_height = h,
            None => {}
        }

        row_heights.push(max_height);
        row_cell_layouts.push(entries);
    }

    // §17.4.85: distribute vMerge overflow.
    expand_rows_for_vmerge(rows, &row_cell_layouts, &mut row_heights);

    // Compute border gaps and assemble measured rows.
    let measured_rows: Vec<MeasuredRow> = row_cell_layouts
        .into_iter()
        .zip(resolved_borders)
        .zip(row_heights.iter())
        .enumerate()
        .map(|(row_idx, ((entries, borders), &height))| {
            let border_gap_below = if row_idx + 1 < num_rows {
                borders
                    .iter()
                    .map(|b| border_width(b.bottom))
                    .fold(Pt::ZERO, Pt::max)
            } else {
                Pt::ZERO
            };
            MeasuredRow {
                entries,
                borders,
                height,
                border_gap_below,
            }
        })
        .collect();

    MeasuredTable {
        rows: measured_rows,
        table_width,
    }
}

#[cfg(test)]
mod tests {
    use super::super::types::{
        CellBorderConfig, CellBorderOverride, CellVAlign, TableBorderConfig, TableBorderLine,
        TableBorderStyle, TableCellInput, TableRowInput,
    };
    use super::measure_table_rows;
    use crate::render::dimension::Pt;
    use crate::render::geometry::PtEdgeInsets;
    use crate::render::resolve::color::RgbColor;

    fn single(w: f32) -> TableBorderLine {
        TableBorderLine {
            width: Pt::new(w),
            color: RgbColor::BLACK,
            style: TableBorderStyle::Single,
        }
    }

    /// A table style like `Tabellenraster`: every side plus insideH/insideV.
    fn all_single() -> TableBorderConfig {
        let s = single(0.5);
        TableBorderConfig {
            top: Some(s),
            bottom: Some(s),
            left: Some(s),
            right: Some(s),
            inside_h: Some(s),
            inside_v: Some(s),
        }
    }

    fn cb(top: Option<CellBorderOverride>, bottom: Option<CellBorderOverride>) -> CellBorderConfig {
        CellBorderConfig {
            top,
            bottom,
            left: None,
            right: None,
        }
    }

    fn cell(span: u32, borders: Option<CellBorderConfig>) -> TableCellInput {
        TableCellInput {
            blocks: vec![],
            margins: PtEdgeInsets::ZERO,
            grid_span: span,
            shading: None,
            cell_borders: borders,
            vertical_merge: None,
            vertical_align: CellVAlign::Top,
        }
    }

    fn row(cells: Vec<TableCellInput>) -> TableRowInput {
        TableRowInput {
            cells,
            height_rule: None,
            is_header: None,
            cant_split: None,
            grid_before: 0,
            grid_after: 0,
            border_overrides: None,
        }
    }

    /// §17.4.43 regression: a `gridSpan` upper cell facing several lower
    /// cells must not drop the later cells' top borders (previously only the
    /// first lower cell was resolved and the rest nulled), and the whole
    /// shared edge must be drawn from a single side so the line does not
    /// split across two y positions. Mirrors the real doc's
    /// `[spacer | Function: | Qualitätssicherung]` row under a `gridSpan`
    /// header.
    #[test]
    fn wide_upper_cell_draws_whole_edge_from_lower_row() {
        let s = single(0.5);
        let rows = vec![
            // Row 0: gridSpan=2 header (bottom nil) over spacer+Function,
            // then two single cells over the Qualitätssicherung span.
            row(vec![
                cell(2, Some(cb(None, Some(CellBorderOverride::Nil)))),
                cell(1, None),
                cell(1, None),
            ]),
            // Row 1: [nil spacer | Function (single top) | Q (gridSpan=2)].
            row(vec![
                cell(1, Some(cb(Some(CellBorderOverride::Nil), None))),
                cell(1, Some(cb(Some(CellBorderOverride::Border(s)), None))),
                cell(2, None),
            ]),
        ];
        let cols = vec![Pt::new(100.0); 4];
        let m = measure_table_rows(
            &rows,
            &cols,
            Pt::new(10.0),
            Some(&all_single()),
            None,
            false,
        );

        // Whole edge drawn from the lower row → every upper bottom cleared,
        // so Function and Qualitätssicherung tops share one y position.
        for b in &m.rows[0].borders {
            assert_eq!(b.bottom, None, "upper bottoms cleared (edge owned below)");
        }
        assert_eq!(m.rows[1].borders[0].top, None, "spacer keeps no top border");
        assert_eq!(
            m.rows[1].borders[1].top,
            Some(s),
            "Function keeps its top border across the gridSpan mismatch"
        );
        assert_eq!(
            m.rows[1].borders[2].top,
            Some(s),
            "Qualitätssicherung top drawn from the same (lower) side as Function"
        );
    }

    /// §17.4.43 regression: a nil spacer among the cells above a wide
    /// `gridSpan` cell must not punch a gap through that cell's top border —
    /// the spacer's edge resolves to the wide cell's inherited insideH.
    #[test]
    fn nil_spacer_above_wide_cell_leaves_no_gap() {
        let s = single(0.5);
        let rows = vec![
            // Row 0: [inherits single | nil spacer | inherits single].
            row(vec![
                cell(1, None),
                cell(1, Some(cb(None, Some(CellBorderOverride::Nil)))),
                cell(1, None),
            ]),
            // Row 1: one gridSpan=3 cell inheriting insideH as its top.
            row(vec![cell(3, None)]),
        ];
        let cols = vec![Pt::new(100.0), Pt::new(100.0), Pt::new(100.0)];
        let m = measure_table_rows(
            &rows,
            &cols,
            Pt::new(10.0),
            Some(&all_single()),
            None,
            false,
        );

        // Every upper bottom carries a single border → continuous, no gap.
        assert_eq!(m.rows[0].borders[0].bottom, Some(s));
        assert_eq!(
            m.rows[0].borders[1].bottom,
            Some(s),
            "nil spacer bottom filled from the wide cell's top border"
        );
        assert_eq!(m.rows[0].borders[2].bottom, Some(s));
        // The wide lower cell's top is drawn once from above.
        assert_eq!(m.rows[1].borders[0].top, None);
    }

    /// §17.4.43 regression: an upper `gridSpan` cell that leaves the last
    /// column uncovered (its gridAfter gap) must not "own" the edge, or a
    /// lower cell straddling that boundary would draw its own top over the
    /// upper bottom → a doubled line. Mirrors the real doc's `gridSpan=9`
    /// section row above the `Observations` (`gridSpan=2`) header.
    #[test]
    fn upper_grid_after_gap_yields_edge_to_lower_row() {
        let s = single(0.5);
        let rows = vec![
            // Row 0: one gridSpan=2 cell over cols 0-1; col 2 is its gridAfter.
            row(vec![cell(2, None)]),
            // Row 1: [cell | gridSpan=2 cell straddling covered col 1 + col 2].
            row(vec![cell(1, None), cell(2, None)]),
        ];
        let cols = vec![Pt::new(100.0); 3];
        let m = measure_table_rows(
            &rows,
            &cols,
            Pt::new(10.0),
            Some(&all_single()),
            None,
            false,
        );

        // Upper can't cover col 2, so the lower row owns the whole edge:
        // its bottom is cleared (no doubling), the lower tops carry the line.
        assert_eq!(
            m.rows[0].borders[0].bottom, None,
            "upper bottom cleared so the straddling lower cell isn't doubled"
        );
        assert_eq!(m.rows[1].borders[0].top, Some(s));
        assert_eq!(m.rows[1].borders[1].top, Some(s));
    }

    /// Aligned grids keep the pre-existing "upper cell owns the shared edge"
    /// behaviour: the lower cell's top is cleared, the upper bottom carries it.
    #[test]
    fn aligned_grid_upper_cell_owns_horizontal_edge() {
        let s = single(0.5);
        let rows = vec![
            row(vec![cell(1, None), cell(1, None)]),
            row(vec![cell(1, None), cell(1, None)]),
        ];
        let cols = vec![Pt::new(100.0), Pt::new(100.0)];
        let m = measure_table_rows(
            &rows,
            &cols,
            Pt::new(10.0),
            Some(&all_single()),
            None,
            false,
        );
        for ci in 0..2 {
            assert_eq!(m.rows[0].borders[ci].bottom, Some(s));
            assert_eq!(m.rows[1].borders[ci].top, None);
        }
    }
}
