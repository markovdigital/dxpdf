use crate::model::{self, Block, Table, TableCell};
use crate::render::dimension::Pt;
use crate::render::geometry;
use crate::render::layout::paragraph::DropCapInfo;
use crate::render::layout::section::LayoutBlock;
use crate::render::layout::table::{
    compute_column_widths, CellBorderConfig, CellBorderOverride, TableCellInput, TableRowInput,
};
use crate::render::resolve::color::{resolve_color, ColorContext};
use crate::render::resolve::conditional::{
    resolve_cell_conditional, CellConditionalFormatting, CellGridPosition,
};
use crate::render::resolve::styles::ResolvedStyle;

use super::block::build_paragraph_block;
use super::convert::{
    convert_cell_border_override, convert_table_border_config, merge_table_borders,
    split_oversized_fragments,
};
use super::{BuildContext, BuildState};

/// Result of building a table from the model.
pub(super) struct BuiltTable {
    pub(super) rows: Vec<TableRowInput>,
    pub(super) col_widths: Vec<Pt>,
    pub(super) border_config: Option<crate::render::layout::table::TableBorderConfig>,
    /// §17.4.51: table indentation from left margin.
    pub(super) indent: Pt,
    /// §17.4.28: table horizontal alignment (left/center/right).
    pub(super) alignment: Option<model::Alignment>,
    pub(super) float_info: Option<super::super::section::TableFloatInfo>,
}

/// Recursively build a table: resolve styles, conditional formatting, and
/// recurse into each cell's content blocks.
pub(super) fn build_table(
    t: &Table,
    available_width: Pt,
    ctx: &BuildContext,
    state: &mut BuildState,
) -> BuiltTable {
    // §17.4.14: grid column widths.
    let num_cols = if t.grid.is_empty() {
        t.rows.iter().map(|r| r.cells.len()).max().unwrap_or(0)
    } else {
        t.grid.len()
    };
    let grid_cols: Vec<Pt> = t.grid.iter().map(|g| Pt::from(g.width)).collect();

    // §17.7.6: table style for conditional formatting, borders, cell margins.
    let raw_table_style = t
        .properties
        .style_id
        .as_ref()
        .and_then(|sid| ctx.resolved.styles.get(sid));

    // §17.4.42: default cell margins from table style cascade.
    let style_cell_margins = raw_table_style
        .and_then(|s| s.table.as_ref())
        .and_then(|tp| tp.cell_margins);
    // Per-edge merge: direct tblCellMar overrides style per-edge, with
    // unspecified edges (value 0) falling back to the style's value.
    // Word merges per-edge rather than replacing the entire set.
    let default_cell_margins = match (t.properties.cell_margins, style_cell_margins) {
        (Some(direct), Some(style)) => {
            use crate::model::geometry::EdgeInsets;
            Some(EdgeInsets {
                top: if direct.top.raw() != 0 {
                    direct.top
                } else {
                    style.top
                },
                bottom: if direct.bottom.raw() != 0 {
                    direct.bottom
                } else {
                    style.bottom
                },
                left: if direct.left.raw() != 0 {
                    direct.left
                } else {
                    style.left
                },
                right: if direct.right.raw() != 0 {
                    direct.right
                } else {
                    style.right
                },
            })
        }
        (Some(direct), None) => Some(direct),
        (None, Some(style)) => Some(style),
        (None, None) => None,
    };

    // §17.4.63: resolve table width from tblW.
    let is_auto_width = matches!(
        t.properties.width,
        None | Some(model::TableMeasure::Auto) | Some(model::TableMeasure::Nil)
    );
    let cell_margins_h = default_cell_margins
        .map(|m| Pt::from(m.left) + Pt::from(m.right))
        .unwrap_or(Pt::ZERO);
    // §17.4.63 / Word heuristic: a full-width left-aligned table extends
    // beyond the body content area by its cell margins so cell content
    // aligns with surrounding paragraph text. Centered/right-aligned tables
    // are positioned as a unit — extending them would just shift them out
    // by half the margins, producing a width discrepancy when consecutive
    // centered tables have different cell margins (cf. the stacked
    // "Anhang: Sauberkeit" tables in the Volvo Annahme-Protokoll).
    let extends_for_alignment = !matches!(
        t.properties.alignment,
        Some(model::Alignment::Center) | Some(model::Alignment::End)
    );
    let target_width = match t.properties.width {
        Some(model::TableMeasure::Pct(pct)) => {
            // §17.4.63: percentage in fiftieths of a percent. 5000 = 100%.
            let ratio = pct.raw() as f32 / 5000.0;
            let base = if pct.raw() >= 5000 && extends_for_alignment {
                available_width + cell_margins_h
            } else {
                available_width
            };
            base * ratio
        }
        Some(model::TableMeasure::Twips(tw)) => Pt::from(tw),
        _ => available_width, // auto/nil: use grid cols or available width
    };
    // §17.4.53: tblLayout controls whether columns may auto-resize to fit
    // content; it does not override the preferred table width from tblW.
    // Word scales grid column widths proportionally to match tblW in both
    // fixed and auto layouts. Only when tblW is auto/nil do we keep the raw
    // grid widths (no preferred width was specified).
    let col_widths = if is_auto_width && !grid_cols.is_empty() {
        grid_cols.clone()
    } else {
        compute_column_widths(&grid_cols, num_cols, target_width)
    };
    let style_overrides = raw_table_style
        .map(|s| s.table_style_overrides.as_slice())
        .unwrap_or(&[]);
    let tbl_look = t.properties.look.as_ref();
    let row_band_size = t.properties.style_row_band_size.unwrap_or(1);
    let col_band_size = t.properties.style_col_band_size.unwrap_or(1);
    let num_rows = t.rows.len();

    // §17.4.38: resolve table borders — merge direct properties over table style.
    // Direct tblBorders may specify only a subset of edges (e.g. insideH=none);
    // unspecified edges inherit from the table style. Computed up front so
    // per-row tblPrEx merges (§17.4.61) below have a stable basis.
    let style_borders = raw_table_style
        .and_then(|s| s.table.as_ref())
        .and_then(|tp| tp.borders.as_ref());
    let tbl_borders = match (t.properties.borders.as_ref(), style_borders) {
        (Some(direct), Some(style)) => Some(merge_table_borders(direct, style)),
        (Some(direct), None) => Some(*direct),
        (None, Some(style)) => Some(*style),
        (None, None) => None,
    };
    let border_config = tbl_borders.as_ref().map(convert_table_border_config);

    // Build rows by iterating cells and recursing into their content.
    let rows: Vec<TableRowInput> = t
        .rows
        .iter()
        .enumerate()
        .map(|(row_idx, row)| {
            let num_cells = row.cells.len();
            let cells: Vec<TableCellInput> = row
                .cells
                .iter()
                .enumerate()
                .map(|(col_idx, cell)| {
                    let cond = resolve_cell_conditional(
                        &CellGridPosition {
                            row_idx,
                            col_idx,
                            num_rows,
                            num_cols: num_cells,
                            row_band_size,
                            col_band_size,
                        },
                        tbl_look,
                        style_overrides,
                    );

                    // Compute available width for nested content.
                    // §17.4.17: gridBefore offsets the row's first cell to the
                    // right by that many grid columns; subsequent spans accumulate.
                    let span = cell.properties.grid_span.unwrap_or(1) as usize;
                    let mut grid_start = row.properties.grid_before as usize;
                    for ci in 0..col_idx {
                        grid_start += row.cells[ci].properties.grid_span.unwrap_or(1) as usize;
                    }
                    let grid_end = (grid_start + span).min(col_widths.len());
                    let cell_width: Pt = col_widths[grid_start..grid_end].iter().copied().sum();
                    // Per-side cascade against the table default (see
                    // `build_table_cell` for the spec rationale): the horizontal
                    // padding contribution is the resolved left+right insets.
                    let table_default =
                        default_cell_margins.unwrap_or(crate::model::geometry::EdgeInsets::ZERO);
                    let resolved_h = match cell.properties.margins {
                        Some(partial) => partial.resolve_against(table_default),
                        None => table_default,
                    };
                    let cell_margins_h = Pt::from(resolved_h.left) + Pt::from(resolved_h.right);
                    let inner_width = (cell_width - cell_margins_h).max(Pt::ZERO);

                    build_table_cell(
                        cell,
                        raw_table_style,
                        default_cell_margins,
                        &cond,
                        inner_width,
                        ctx,
                        state,
                    )
                })
                .collect();

            // Word/LibreOffice row-uniform content-area quirk — see
            // `normalize_row_uniform_vertical_insets` for the spec gap and
            // empirical evidence motivating this pass.
            let mut cells = cells;
            normalize_row_uniform_vertical_insets(&mut cells);

            TableRowInput {
                cells,
                height_rule: row.properties.height.map(|h| {
                    use crate::model::HeightRule;
                    use crate::render::layout::table::RowHeightRule;
                    match h.rule {
                        HeightRule::Exact => RowHeightRule::Exact(Pt::from(h.value)),
                        _ => RowHeightRule::AtLeast(Pt::from(h.value)),
                    }
                }),
                is_header: row.properties.is_header,
                cant_split: row.properties.cant_split,
                grid_before: row.properties.grid_before,
                grid_after: row.properties.grid_after,
                // §17.4.61: row-level tblPrEx.tblBorders — per-side
                // override of the table's effective borders. We merge
                // *at the model layer* (Option<Border> with style=None
                // is preserved), then convert to layout — that keeps
                // the spec's "specified as none" vs "not specified"
                // distinction that converting first would erase.
                border_overrides: row
                    .property_exceptions
                    .as_ref()
                    .and_then(|ex| ex.borders.as_ref())
                    .map(|over| {
                        let merged = match tbl_borders.as_ref() {
                            Some(table) => merge_table_borders(over, table),
                            None => *over,
                        };
                        convert_table_border_config(&merged)
                    }),
            }
        })
        .collect();

    // §17.4.58: floating table positioning.
    let float_info = t.properties.positioning.as_ref().map(|pos| {
        super::super::section::TableFloatInfo {
            right_gap: pos.right_from_text.map(Pt::from).unwrap_or(Pt::ZERO),
            bottom_gap: pos.bottom_from_text.map(Pt::from).unwrap_or(Pt::ZERO),
            x_align: pos.x_align,
            // §17.4.59: tblpY — absolute Y offset from the vertical anchor.
            y_offset: pos.y.map(Pt::from).unwrap_or(Pt::ZERO),
            // §17.4.58: default vertical anchor is "text".
            vert_anchor: pos.vert_anchor.unwrap_or(crate::model::TableAnchor::Text),
        }
    });

    // §17.4.51: table indentation from left margin.
    // For full-width left-aligned tables, MS Word shifts the table left
    // by the default cell margin so cell content aligns with paragraph text.
    let is_full_width = matches!(
        t.properties.width,
        Some(model::TableMeasure::Pct(pct)) if pct.raw() >= 5000
    );
    let is_left_aligned = !matches!(
        t.properties.alignment,
        Some(model::Alignment::Center) | Some(model::Alignment::End)
    );
    let indent = match t.properties.indent {
        Some(model::TableMeasure::Twips(tw)) => Pt::from(tw),
        _ if is_full_width && is_left_aligned => -default_cell_margins
            .map(|m| Pt::from(m.left))
            .unwrap_or(Pt::ZERO),
        _ => Pt::ZERO,
    };

    BuiltTable {
        rows,
        col_widths,
        border_config,
        indent,
        alignment: t.properties.alignment,
        float_info,
    }
}

/// Word/LibreOffice row layout quirk: top/bottom cell margins are normalized
/// to the row-wide maximum across all cells in the row, while left/right stay
/// per-cell.
///
/// # Spec relationship
///
/// ECMA-376 §17.4.42 (`tcMar`, "Single Table Cell Margins") defines the cell
/// margin as a per-side exception over §17.4.44 (`tblCellMar`, the table-level
/// default). Each side is a `CT_TblWidth` (§17.18.87), where `@type="dxa"
/// @w="N"` is an explicit `N`-twip value. The spec is silent on how
/// *neighbouring* cells in the same row interact when their per-cell margins
/// disagree — there is no row-level "content area" concept defined in
/// §17.4.78 (`tr`) or §17.4.79 (`trHeight`).
///
/// The de-facto behaviour of every mainstream renderer (Word and LibreOffice
/// Writer in particular) is to compute a row-uniform content inset:
///
/// ```text
/// row.uniform_top    = max(cell.tcMar.top    for cell in row)
/// row.uniform_bottom = max(cell.tcMar.bottom for cell in row)
/// ```
///
/// and to position every cell's content within that uniform inset regardless
/// of the cell's own per-cell override. Without this pass, a cell with an
/// explicit `tcMar.top=0` in a row whose siblings inherit a larger value sits
/// flush against the row's top border while its neighbours sit padded — a
/// positioning no mainstream editor produces.
///
/// # Empirical basis
///
/// Verified against the `Wohnungsübergabeprotokoll` sample (LibreOffice
/// origin) by editing the DOCX directly and observing Word's render:
///
/// 1. Rewriting all `<w:tcMar><w:top w:w="0"/></w:tcMar>` to
///    `<w:top w:w="1"/>` produced **byte-equivalent visual output** — Word
///    is invariant to the literal value at this magnitude, ruling out
///    "Word treats `w=0` as no-override" as the explanation.
/// 2. Rewriting just one cell's `<w:tcMar>` to `<w:top w:w="500"/>` (≈ 25pt)
///    pushed **every** cell in that row down by ~25pt, confirming the
///    row-wide max(...) discipline.
///
/// # Scope
///
/// Only `top` and `bottom` are normalized. `left` and `right` stay per-cell
/// because each column's content width is independent — there is no shared
/// row-wide horizontal area in the de-facto layout, and editors do honour
/// per-cell horizontal overrides.
///
/// Applied at the build layer (post per-side `<w:tcMar>`/`<w:tblCellMar>`
/// cascade resolution) so downstream measure/emit/split code sees uniform
/// vertical insets and needs no further changes.
fn normalize_row_uniform_vertical_insets(cells: &mut [TableCellInput]) {
    let max_top = cells.iter().fold(Pt::ZERO, |acc, c| acc.max(c.margins.top));
    let max_bottom = cells
        .iter()
        .fold(Pt::ZERO, |acc, c| acc.max(c.margins.bottom));
    for cell in cells {
        cell.margins.top = max_top;
        cell.margins.bottom = max_bottom;
    }
}

/// Build a single table cell: resolve content blocks, margins, shading, borders.
fn build_table_cell(
    cell: &TableCell,
    table_style: Option<&ResolvedStyle>,
    style_cell_margins: Option<crate::model::geometry::EdgeInsets<crate::model::dimension::Twips>>,
    cond: &CellConditionalFormatting,
    inner_width: Pt,
    ctx: &BuildContext,
    state: &mut BuildState,
) -> TableCellInput {
    // §17.4.42: cell margins cascade *per side* against the pre-merged
    // table default. A cell-level `<w:tcMar>` that specifies only some sides
    // (e.g. `top`/`bottom` only — common in LibreOffice output) must inherit
    // the remaining sides from `<w:tblCellMar>` rather than zeroing them out;
    // collapsing missing sides to 0 produces text that hugs the cell borders
    // instead of carrying the table's intended padding.
    let table_default = style_cell_margins.unwrap_or(crate::model::geometry::EdgeInsets::ZERO);
    let resolved_margins = match cell.properties.margins {
        Some(partial) => partial.resolve_against(table_default),
        None => table_default,
    };
    let cell_margins = geometry::PtEdgeInsets::new(
        Pt::from(resolved_margins.top),
        Pt::from(resolved_margins.right),
        Pt::from(resolved_margins.bottom),
        Pt::from(resolved_margins.left),
    );

    // §17.7.6: resolve cell shading.  Priority: direct → conditional → none.
    let shading = cell
        .properties
        .shading
        .map(|s| resolve_color(s.fill, ColorContext::Background))
        .or_else(|| {
            cond.cell_properties
                .as_ref()
                .and_then(|tcp| tcp.shading.as_ref())
                .map(|s| resolve_color(s.fill, ColorContext::Background))
        });

    // §17.4.66: cell borders cascade — direct cell borders (highest priority)
    // → conditional formatting → table-level borders (resolved in layout).
    let cond_borders = cond
        .cell_properties
        .as_ref()
        .and_then(|tcp| tcp.borders.as_ref());
    let direct_borders = cell.properties.borders.as_ref();

    let cell_borders = match (direct_borders, cond_borders) {
        (Some(db), _) => {
            // Direct cell borders: highest priority.  Fall through to
            // conditional for edges not specified directly.
            Some(CellBorderConfig {
                top: convert_cell_border_override(&db.top)
                    .or_else(|| cond_borders.and_then(|cb| convert_cell_border_override(&cb.top))),
                bottom: convert_cell_border_override(&db.bottom).or_else(|| {
                    cond_borders.and_then(|cb| convert_cell_border_override(&cb.bottom))
                }),
                left: convert_cell_border_override(&db.left)
                    .or_else(|| cond_borders.and_then(|cb| convert_cell_border_override(&cb.left))),
                right: convert_cell_border_override(&db.right).or_else(|| {
                    cond_borders.and_then(|cb| convert_cell_border_override(&cb.right))
                }),
            })
        }
        (None, Some(cb)) => Some(CellBorderConfig {
            top: convert_cell_border_override(&cb.top),
            bottom: convert_cell_border_override(&cb.bottom),
            left: convert_cell_border_override(&cb.left),
            right: convert_cell_border_override(&cb.right),
        }),
        (None, None) => None,
    };

    // §17.4.84: vertical alignment — direct cell, conditional, or default top.
    let valign = cell
        .properties
        .vertical_align
        .or_else(|| {
            cond.cell_properties
                .as_ref()
                .and_then(|tcp| tcp.vertical_align)
        })
        .map(|va| match va {
            model::CellVerticalAlign::Bottom => crate::render::layout::table::CellVAlign::Bottom,
            model::CellVerticalAlign::Center => crate::render::layout::table::CellVAlign::Center,
            _ => crate::render::layout::table::CellVAlign::Top,
        })
        .unwrap_or(crate::render::layout::table::CellVAlign::Top);

    // Estimate border insets to compute effective content width for
    // character-level splitting of oversized fragments.
    let border_w = |ovr: &Option<CellBorderOverride>| -> Pt {
        match ovr {
            Some(CellBorderOverride::Border(b)) => b.width,
            _ => Pt::ZERO,
        }
    };
    let border_inset_h = cell_borders
        .as_ref()
        .map(|cb| {
            let bl = (border_w(&cb.left) - cell_margins.left).max(Pt::ZERO);
            let br = (border_w(&cb.right) - cell_margins.right).max(Pt::ZERO);
            bl + br
        })
        .unwrap_or(Pt::ZERO);
    let content_width = (inner_width - border_inset_h).max(Pt::ZERO);

    // Recurse into cell content blocks.
    let cell_blocks =
        build_cell_blocks(&cell.content, table_style, cond, content_width, ctx, state);

    TableCellInput {
        blocks: cell_blocks,
        margins: cell_margins,
        grid_span: cell.properties.grid_span.unwrap_or(1),
        shading,
        cell_borders,
        vertical_merge: cell.properties.vertical_merge.map(|vm| match vm {
            model::VerticalMerge::Restart => {
                crate::render::layout::table::VerticalMergeState::Restart
            }
            model::VerticalMerge::Continue => {
                crate::render::layout::table::VerticalMergeState::Continue
            }
        }),
        vertical_align: valign,
    }
}

/// Recursively build cell content blocks.
///
/// Paragraphs are resolved with table style + conditional overrides.
/// Nested tables recurse via `build_table()` → `layout_table()`.
fn build_cell_blocks(
    content: &[Block],
    table_style: Option<&ResolvedStyle>,
    cond: &CellConditionalFormatting,
    inner_width: Pt,
    ctx: &BuildContext,
    state: &mut BuildState,
) -> Vec<LayoutBlock> {
    let mut blocks = Vec::new();
    let mut pending_dropcap: Option<DropCapInfo> = None;

    for (i, block) in content.iter().enumerate() {
        match block {
            Block::Paragraph(p) => {
                // §17.4.66: every cell must end with a paragraph. When the
                // last block is an empty paragraph following a table, it is
                // structural — Word renders it with zero height.
                if p.content.is_empty()
                    && i > 0
                    && matches!(content[i - 1], Block::Table(_))
                    && i == content.len() - 1
                {
                    continue;
                }
                if let Some(lb) = build_paragraph_block(
                    p,
                    ctx,
                    state,
                    &mut pending_dropcap,
                    table_style,
                    Some(cond),
                ) {
                    // Split oversized text fragments for narrow cells.
                    let lb = if let LayoutBlock::Paragraph {
                        fragments,
                        style,
                        page_break_before,
                        footnotes,
                        floating_images,
                        floating_shapes,
                    } = lb
                    {
                        let fragments = split_oversized_fragments(fragments, inner_width, ctx);
                        LayoutBlock::Paragraph {
                            fragments,
                            style,
                            page_break_before,
                            footnotes,
                            floating_images,
                            floating_shapes,
                        }
                    } else {
                        lb
                    };
                    blocks.push(lb);
                }
            }
            Block::Table(nested_t) => {
                let built = build_table(nested_t, inner_width, ctx, state);
                blocks.push(LayoutBlock::Table {
                    rows: built.rows,
                    col_widths: built.col_widths,
                    border_config: built.border_config,
                    indent: built.indent,
                    alignment: built.alignment,
                    float_info: built.float_info,
                    style_id: nested_t.properties.style_id.clone(),
                });
            }
            _ => {}
        }
    }

    blocks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::layout::table::{CellVAlign, TableCellInput};

    fn cell_with_margins(top: f32, right: f32, bottom: f32, left: f32) -> TableCellInput {
        TableCellInput {
            blocks: vec![],
            margins: geometry::PtEdgeInsets::new(
                Pt::new(top),
                Pt::new(right),
                Pt::new(bottom),
                Pt::new(left),
            ),
            grid_span: 1,
            shading: None,
            cell_borders: None,
            vertical_merge: None,
            vertical_align: CellVAlign::Top,
        }
    }

    /// Word/Writer row-uniform content-area normalization: when cells in a row
    /// disagree on `tcMar.top` (or `bottom`), the row-wide *maximum* wins for
    /// every cell. Verified empirically against MS Word — see
    /// [`normalize_row_uniform_vertical_insets`] for the experimental
    /// reproducer and spec references.
    #[test]
    fn row_uniform_picks_max_top_and_bottom_across_row() {
        // Mirrors the Wohnungsübergabe "Keller" row: one cell with
        // explicit zero top/bottom (the Keller cell after per-side tcMar
        // cascade) and another with the inherited 57-twip table default
        // (≈ 2.85 pt) — siblings without a tcMar override.
        let mut cells = vec![
            cell_with_margins(0.0, 5.4, 0.0, 5.15),   // Keller-like
            cell_with_margins(2.85, 5.4, 2.85, 5.15), // sibling with table default
            cell_with_margins(0.0, 5.4, 0.0, 5.15),   // another Keller-like
        ];
        normalize_row_uniform_vertical_insets(&mut cells);

        for (i, cell) in cells.iter().enumerate() {
            assert_eq!(
                cell.margins.top.raw(),
                2.85,
                "cell #{i}: every cell's top inset must equal the row-wide max"
            );
            assert_eq!(
                cell.margins.bottom.raw(),
                2.85,
                "cell #{i}: every cell's bottom inset must equal the row-wide max"
            );
            // Horizontal stays per-cell.
            assert_eq!(cell.margins.left.raw(), 5.15, "left is per-cell");
            assert_eq!(cell.margins.right.raw(), 5.4, "right is per-cell");
        }
    }

    /// Mirrors the `_kellertop500` experiment: a single cell with a `tcMar.top`
    /// far larger than its siblings becomes the row-wide max, so every cell —
    /// including the ones with no override — picks up that large top inset.
    /// In Word, this is observable as the entire row's content shifting down.
    #[test]
    fn row_uniform_one_large_cell_top_pushes_whole_row_down() {
        let mut cells = vec![
            cell_with_margins(25.0, 5.4, 0.0, 5.15), // Keller with top=25pt
            cell_with_margins(2.85, 5.4, 2.85, 5.15), // small sibling
        ];
        normalize_row_uniform_vertical_insets(&mut cells);

        assert_eq!(
            cells[1].margins.top.raw(),
            25.0,
            "sibling cell inherits the large top from the dominating cell"
        );
        // Bottom max is the smaller cell's 2.85 (Keller bottom stayed 0).
        assert_eq!(cells[0].margins.bottom.raw(), 2.85);
    }

    #[test]
    fn row_uniform_single_cell_row_is_noop() {
        let mut cells = vec![cell_with_margins(2.85, 5.4, 2.85, 5.15)];
        let before = cells[0].margins;
        normalize_row_uniform_vertical_insets(&mut cells);
        assert_eq!(cells[0].margins, before);
    }

    #[test]
    fn row_uniform_handles_empty_row() {
        let mut cells: Vec<TableCellInput> = vec![];
        normalize_row_uniform_vertical_insets(&mut cells);
        // No panic, no work done.
        assert!(cells.is_empty());
    }
}
