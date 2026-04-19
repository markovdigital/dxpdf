//! Row splitting for page pagination.
//!
//! §17.4.1: a row without `cantSplit` may have its content broken across
//! page boundaries. This module derives safe cut points from a row's
//! already-laid-out cell commands and partitions those commands into a
//! first slice (stays on the current page) and a second slice (flows to
//! the next page, rebased to y=0).

use crate::render::dimension::Pt;
use crate::render::layout::draw_command::DrawCommand;

use super::borders::CellBorders;
use super::types::{CellLayoutEntry, MeasuredRow, TableRowInput};

/// Options for finding a row cut.
pub(super) struct RowCutInput<'a> {
    pub(super) mr: &'a MeasuredRow,
    pub(super) row: &'a TableRowInput,
    /// Space available for the row on the current page, measured from the
    /// row's top edge. Excludes any top-border width for the row itself —
    /// callers pass the usable content space.
    pub(super) available: Pt,
}

/// Pick a Y cut (relative to the row's top edge) such that:
///   - every cell's content that fits `available` stays in slice 1
///   - slice 1 is at least one line tall (otherwise return `None`)
///
/// The cut is the maximum "last-fit baseline + line gap" across cells, so
/// no cell loses a line that could have fit on the current page.
///
/// Returns `None` if **no** cell has any content line that fits. In that
/// case the caller should move the whole row to the next page.
pub(super) fn find_row_cut(input: &RowCutInput<'_>) -> Option<Pt> {
    let mut row_cut = Pt::ZERO;
    let mut any_line_fits = false;

    for (entry, cell) in input.mr.entries.iter().zip(&input.row.cells) {
        let cell_cut = cut_for_cell(entry, cell.margins.top, input.available);
        if let Some(c) = cell_cut {
            any_line_fits = true;
            if c > row_cut {
                row_cut = c;
            }
        }
    }

    if any_line_fits && row_cut < input.available {
        Some(row_cut)
    } else if any_line_fits {
        // row_cut ≥ available means the last-fit line sits exactly on the
        // boundary — still OK to cut there.
        Some(input.available)
    } else {
        None
    }
}

/// For a single cell, determine the largest "line-bottom" Y (in cell-local
/// coordinates, including the cell's top margin) such that all content up
/// to that Y fits in `available`.
///
/// Lines are identified by collecting the baseline Y values of `Text`
/// commands in the cell layout. The line bottom is approximated as the
/// midpoint between consecutive baselines, with the last line's bottom
/// taken as the cell's total content height.
fn cut_for_cell(entry: &CellLayoutEntry, margin_top: Pt, available: Pt) -> Option<Pt> {
    // Baselines of all text lines in this cell, sorted ascending.
    let mut baselines: Vec<Pt> = entry
        .layout
        .commands
        .iter()
        .filter_map(|c| match c {
            DrawCommand::Text { position, .. } => Some(position.y),
            _ => None,
        })
        .collect();
    baselines.sort_by(|a, b| {
        a.raw()
            .partial_cmp(&b.raw())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    baselines.dedup_by(|a, b| (a.raw() - b.raw()).abs() < 0.01);

    if baselines.is_empty() {
        // Empty cell: any cut height is OK. Return 0 (contributes nothing).
        return Some(Pt::ZERO);
    }

    // Compute the "bottom" of each line: midpoint to the next baseline,
    // or the cell's content height for the last line.
    let cell_bottom = margin_top + entry.layout.content_height;
    let mut line_bottoms: Vec<Pt> = Vec::with_capacity(baselines.len());
    for i in 0..baselines.len() {
        let b = if i + 1 < baselines.len() {
            Pt::new((baselines[i].raw() + baselines[i + 1].raw()) * 0.5)
        } else {
            cell_bottom
        };
        line_bottoms.push(b);
    }

    // Find the largest line_bottom that fits in `available`.
    let mut best: Option<Pt> = None;
    for b in &line_bottoms {
        if *b <= available {
            best = Some(*b);
        } else {
            break;
        }
    }
    best
}

/// A row cut into two halves at a common Y. Each half is a full
/// `MeasuredRow` ready to pass back to `emit_table_rows`.
pub(super) struct SplitRow {
    pub(super) first: MeasuredRow,
    pub(super) second: MeasuredRow,
}

/// Split a row's cells at `cut_y` (relative to the row's top). Commands
/// whose "primary Y" is strictly below `cut_y` go to the first half;
/// remaining commands are re-based so the second half starts at y=0.
///
/// Borders: the first half loses its bottom border; the second half loses
/// its top border. The logical cell continues unbroken — the cut edge is
/// drawn as no border on either side.
pub(super) fn split_row_at(mr: &MeasuredRow, cut_y: Pt) -> SplitRow {
    let mut first_entries: Vec<CellLayoutEntry> = Vec::with_capacity(mr.entries.len());
    let mut second_entries: Vec<CellLayoutEntry> = Vec::with_capacity(mr.entries.len());

    let total_h = mr.height;
    let first_h = cut_y;
    let second_h = (total_h - cut_y).max(Pt::ZERO);

    for entry in &mr.entries {
        let (first_cmds, second_cmds) = partition_commands(&entry.layout.commands, cut_y);
        first_entries.push(CellLayoutEntry {
            layout: crate::render::layout::cell::CellLayout {
                commands: first_cmds,
                content_height: (entry.layout.content_height).min(first_h),
            },
            cell_x: entry.cell_x,
            cell_w: entry.cell_w,
            grid_col: entry.grid_col,
        });
        second_entries.push(CellLayoutEntry {
            layout: crate::render::layout::cell::CellLayout {
                commands: second_cmds,
                content_height: (entry.layout.content_height - first_h).max(Pt::ZERO),
            },
            cell_x: entry.cell_x,
            cell_w: entry.cell_w,
            grid_col: entry.grid_col,
        });
    }

    // Borders: drop bottom on first, drop top on second.
    let first_borders: Vec<CellBorders> = mr
        .borders
        .iter()
        .map(|b| CellBorders {
            top: b.top,
            bottom: None,
            left: b.left,
            right: b.right,
        })
        .collect();
    let second_borders: Vec<CellBorders> = mr
        .borders
        .iter()
        .map(|b| CellBorders {
            top: None,
            bottom: b.bottom,
            left: b.left,
            right: b.right,
        })
        .collect();

    SplitRow {
        first: MeasuredRow {
            entries: first_entries,
            borders: first_borders,
            height: first_h,
            // No border gap beneath the first half — it sits at the page
            // bottom. Any sibling row below starts on the next page.
            border_gap_below: Pt::ZERO,
        },
        second: MeasuredRow {
            entries: second_entries,
            borders: second_borders,
            height: second_h,
            border_gap_below: mr.border_gap_below,
        },
    }
}

/// Split a command list at `cut_y` based on each command's primary Y.
/// Commands with primary_y < cut_y go to the first half; others go to the
/// second half with y shifted up by `cut_y`.
fn partition_commands(commands: &[DrawCommand], cut_y: Pt) -> (Vec<DrawCommand>, Vec<DrawCommand>) {
    let mut first = Vec::new();
    let mut second = Vec::new();
    for cmd in commands {
        if command_primary_y(cmd) < cut_y {
            first.push(cmd.clone());
        } else {
            let mut c = cmd.clone();
            c.shift_y(-cut_y);
            second.push(c);
        }
    }
    (first, second)
}

/// The Y used to decide which side of the cut a command belongs to. For
/// `Text` we use the baseline; for rect/line/image we use the top edge.
fn command_primary_y(cmd: &DrawCommand) -> Pt {
    match cmd {
        DrawCommand::Text { position, .. } | DrawCommand::NamedDestination { position, .. } => {
            position.y
        }
        DrawCommand::Underline { line, .. } | DrawCommand::Line { line, .. } => line.start.y,
        DrawCommand::Image { rect, .. }
        | DrawCommand::Rect { rect, .. }
        | DrawCommand::LinkAnnotation { rect, .. }
        | DrawCommand::InternalLink { rect, .. } => rect.origin.y,
        DrawCommand::Path { origin, .. } => origin.y,
    }
}
