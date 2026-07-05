//! Section layout — sequence blocks vertically into pages.
//!
//! Takes measured blocks (paragraphs with fragments, tables with cells),
//! fits them into pages respecting page size and margins, handles page breaks.

mod floating_table;
mod helpers;
mod layout;
mod stacker;
mod types;

pub use layout::layout_section;
pub use stacker::{stack_blocks, StackResult};
pub use types::*;

// ── Footnote rendering constants ─────────────────────────────────────────────

/// §17.11.23: footnote separator width as a fraction of the content area.
/// Word renders the separator at one-third of the text column width.
const FOOTNOTE_SEPARATOR_RATIO: f32 = 0.33;

/// Thickness of the footnote separator line (pt).
const FOOTNOTE_SEPARATOR_LINE_WIDTH: crate::render::dimension::Pt =
    crate::render::dimension::Pt::new(0.5);

/// Vertical gap between the footnote separator and the first footnote paragraph.
/// Also used as the initial height budget for the separator region (pt).
const FOOTNOTE_SEPARATOR_GAP: crate::render::dimension::Pt = crate::render::dimension::Pt::new(4.0);

// ── Float deduplication ───────────────────────────────────────────────────────

/// Position tolerance for deduplicating floating images (pt).
/// Two float entries within this distance on every axis are treated as identical.
const FLOAT_DEDUP_EPSILON_PT: f32 = 0.1;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::dimension::Pt;
    use crate::render::geometry::{PtEdgeInsets, PtSize};
    use crate::render::layout::draw_command::DrawCommand;
    use crate::render::layout::fragment::Fragment;
    use crate::render::layout::fragment::{FontProps, TextMetrics};
    use crate::render::layout::page::PageConfig;
    use crate::render::layout::paragraph::ParagraphStyle;
    use crate::render::layout::table::{TableCellInput, TableRowInput};
    use crate::render::resolve::color::RgbColor;
    use std::rc::Rc;

    fn text_frag(text: &str, width: f32, height: f32) -> Fragment {
        Fragment::Text {
            text: text.into(),
            font: Rc::new(FontProps {
                family: Rc::from("Test"),
                size: Pt::new(12.0),
                bold: false,
                italic: false,
                underline: false,
                char_spacing: Pt::ZERO,
                text_scale: 1.0,
                underline_position: Pt::ZERO,
                underline_thickness: Pt::ZERO,
            }),
            color: RgbColor::BLACK,
            width: Pt::new(width),
            trimmed_width: Pt::new(width),
            metrics: TextMetrics {
                ascent: Pt::new(height * 0.7),
                descent: Pt::new(height * 0.3),
                leading: Pt::ZERO,
            },
            hyperlink_url: None,
            shading: None,
            border: None,
            baseline_offset: Pt::ZERO,
            text_offset: Pt::ZERO,
        }
    }

    fn para_block(text: &str, width: f32) -> LayoutBlock {
        LayoutBlock::Paragraph {
            fragments: vec![text_frag(text, width, 14.0)],
            style: ParagraphStyle::default(),
            page_break_before: false,
            footnotes: vec![],
            floating_images: vec![],
            floating_shapes: vec![],
        }
    }

    fn small_config() -> PageConfig {
        use crate::render::layout::page::ColumnGeometry;
        PageConfig {
            page_size: PtSize::new(Pt::new(200.0), Pt::new(100.0)),
            margins: PtEdgeInsets::new(Pt::new(10.0), Pt::new(10.0), Pt::new(10.0), Pt::new(10.0)),
            header_margin: Pt::new(5.0),
            footer_margin: Pt::new(5.0),
            columns: vec![ColumnGeometry {
                x_offset: Pt::ZERO,
                width: Pt::new(180.0),
            }],
        }
    }

    #[test]
    fn empty_blocks_produces_one_empty_page() {
        let pages = layout_section(&[], &small_config(), None, Pt::ZERO, Pt::new(14.0), None);
        assert_eq!(pages.len(), 1);
        assert!(pages[0].commands.is_empty());
    }

    #[test]
    fn single_paragraph_on_one_page() {
        let blocks = vec![para_block("hello", 30.0)];
        let pages = layout_section(
            &blocks,
            &small_config(),
            None,
            Pt::ZERO,
            Pt::new(14.0),
            None,
        );

        assert_eq!(pages.len(), 1);
        let text_count = pages[0]
            .commands
            .iter()
            .filter(|c| matches!(c, DrawCommand::Text { .. }))
            .count();
        assert_eq!(text_count, 1);
    }

    #[test]
    fn text_positioned_at_margins() {
        let blocks = vec![para_block("hello", 30.0)];
        let config = small_config();
        let pages = layout_section(&blocks, &config, None, Pt::ZERO, Pt::new(14.0), None);

        if let Some(DrawCommand::Text { position, .. }) = pages[0].commands.first() {
            assert!(
                position.x.raw() >= config.margins.left.raw(),
                "x should be at least left margin"
            );
            assert!(
                position.y.raw() >= config.margins.top.raw(),
                "y should be at least top margin"
            );
        }
    }

    #[test]
    fn page_break_when_content_overflows() {
        // Page: 100pt tall, margins 10 each → 80pt content area
        // Each paragraph: 14pt tall
        // 6 paragraphs = 84pt > 80pt → should break to 2 pages
        let blocks: Vec<_> = (0..6).map(|i| para_block(&format!("p{i}"), 30.0)).collect();
        let pages = layout_section(
            &blocks,
            &small_config(),
            None,
            Pt::ZERO,
            Pt::new(14.0),
            None,
        );

        assert_eq!(pages.len(), 2, "should overflow to 2 pages");

        let page1_texts: Vec<_> = pages[0]
            .commands
            .iter()
            .filter_map(|c| match c {
                DrawCommand::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        let page2_texts: Vec<_> = pages[1]
            .commands
            .iter()
            .filter_map(|c| match c {
                DrawCommand::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();

        assert_eq!(page1_texts.len(), 5, "5 paras fit on page 1 (5*14=70 < 80)");
        assert_eq!(page2_texts.len(), 1, "1 para on page 2");
    }

    #[test]
    fn page_size_set_on_layouted_page() {
        let config = small_config();
        let pages = layout_section(&[], &config, None, Pt::ZERO, Pt::new(14.0), None);
        assert_eq!(pages[0].page_size, config.page_size);
    }

    #[test]
    fn many_paragraphs_produce_multiple_pages() {
        // 20 paragraphs at 14pt each = 280pt
        // Content area = 80pt → need 4 pages (80/14 = 5.7 paras per page)
        let blocks: Vec<_> = (0..20)
            .map(|i| para_block(&format!("p{i}"), 30.0))
            .collect();
        let pages = layout_section(
            &blocks,
            &small_config(),
            None,
            Pt::ZERO,
            Pt::new(14.0),
            None,
        );

        assert_eq!(pages.len(), 4);
    }

    #[test]
    fn table_on_page() {
        let blocks = vec![LayoutBlock::Table {
            rows: vec![TableRowInput {
                cells: vec![TableCellInput {
                    blocks: vec![LayoutBlock::Paragraph {
                        fragments: vec![text_frag("cell", 30.0, 14.0)],
                        style: ParagraphStyle::default(),
                        page_break_before: false,
                        footnotes: vec![],
                        floating_images: vec![],
                        floating_shapes: vec![],
                    }],
                    margins: PtEdgeInsets::ZERO,
                    grid_span: 1,
                    shading: None,
                    cell_borders: None,
                    vertical_merge: None,
                    vertical_align: crate::render::layout::table::CellVAlign::Top,
                }],
                height_rule: None,
                is_header: None,
                cant_split: None,
                grid_before: 0,
                grid_after: 0,
                border_overrides: None,
            }],
            col_widths: vec![Pt::new(100.0)],
            border_config: None,
            indent: Pt::ZERO,
            alignment: None,
            float_info: None,
            style_id: None,
        }];

        let pages = layout_section(
            &blocks,
            &small_config(),
            None,
            Pt::ZERO,
            Pt::new(14.0),
            None,
        );
        assert_eq!(pages.len(), 1);

        let text_count = pages[0]
            .commands
            .iter()
            .filter(|c| matches!(c, DrawCommand::Text { .. }))
            .count();
        assert_eq!(text_count, 1);
    }

    // ── §17.3.1.33 space_before suppression tests ──────────────────────

    #[test]
    fn space_before_suppressed_for_first_paragraph_of_section() {
        let style = ParagraphStyle {
            space_before: Pt::new(24.0),
            ..Default::default()
        };
        let blocks = vec![LayoutBlock::Paragraph {
            fragments: vec![text_frag("heading", 50.0, 14.0)],
            style,
            page_break_before: false,
            footnotes: vec![],
            floating_images: vec![],
            floating_shapes: vec![],
        }];
        let config = small_config();
        let pages = layout_section(&blocks, &config, None, Pt::ZERO, Pt::new(14.0), None);

        // First paragraph on the section's initial page: space_before suppressed.
        if let Some(DrawCommand::Text { position, .. }) = pages[0].commands.first() {
            assert!(
                position.y.raw() < config.margins.top.raw() + 24.0,
                "space_before should be suppressed: y={}",
                position.y.raw()
            );
        }
    }

    #[test]
    fn space_before_preserved_for_page_break_before() {
        let heading_style = ParagraphStyle {
            space_before: Pt::new(24.0),
            ..Default::default()
        };

        let blocks = vec![
            para_block("first page", 30.0),
            LayoutBlock::Paragraph {
                fragments: vec![text_frag("heading", 50.0, 14.0)],
                style: heading_style,
                page_break_before: true,
                footnotes: vec![],
                floating_images: vec![],
                floating_shapes: vec![],
            },
        ];
        let config = small_config();
        let pages = layout_section(&blocks, &config, None, Pt::ZERO, Pt::new(14.0), None);

        assert!(pages.len() >= 2, "should have at least 2 pages");
        let heading_y = pages[1]
            .commands
            .iter()
            .find_map(|c| match c {
                DrawCommand::Text { position, text, .. } if &**text == "heading" => {
                    Some(position.y)
                }
                _ => None,
            })
            .expect("heading should be on page 2");
        // §17.3.1.33: space_before is preserved — pageBreakBefore paragraphs
        // are not the structural first of the section.
        assert!(
            heading_y.raw() > config.margins.top.raw() + 20.0,
            "space_before should be preserved for pageBreakBefore: y={}",
            heading_y.raw(),
        );
    }

    // ── §17.4.59 — floating-table page overflow ────────────────────────
    //
    // A floating table (`<w:tbl>` with `<w:tblpPr>`) that is taller than
    // the available height on its anchor page must split at row
    // boundaries and continue on the next page. Word draws the
    // continuation at the top of the next page's content area; the
    // `tblpY` anchor applies only to the first slice.

    fn cell_with_text(text: &str) -> TableCellInput {
        TableCellInput {
            blocks: vec![LayoutBlock::Paragraph {
                fragments: vec![text_frag(text, 30.0, 14.0)],
                style: ParagraphStyle::default(),
                page_break_before: false,
                footnotes: vec![],
                floating_images: vec![],
                floating_shapes: vec![],
            }],
            margins: PtEdgeInsets::ZERO,
            grid_span: 1,
            shading: None,
            cell_borders: None,
            vertical_merge: None,
            vertical_align: crate::render::layout::table::CellVAlign::Top,
        }
    }

    fn row_with_label(label: &str) -> TableRowInput {
        TableRowInput {
            cells: vec![cell_with_text(label)],
            height_rule: None,
            is_header: None,
            cant_split: None,
            grid_before: 0,
            grid_after: 0,
            border_overrides: None,
        }
    }

    /// Build a floating table with `n` rows, anchored to the page at
    /// `y_offset`. Used by overflow tests below.
    fn floating_table_with_rows(n: usize, y_offset: f32) -> LayoutBlock {
        LayoutBlock::Table {
            rows: (0..n).map(|i| row_with_label(&format!("r{i}"))).collect(),
            col_widths: vec![Pt::new(100.0)],
            border_config: None,
            indent: Pt::ZERO,
            alignment: None,
            float_info: Some(super::TableFloatInfo {
                right_gap: Pt::ZERO,
                bottom_gap: Pt::ZERO,
                x_align: None,
                y_offset: Pt::new(y_offset),
                vert_anchor: crate::model::TableAnchor::Page,
                overlap: None,
            }),
            style_id: None,
        }
    }

    /// Collect text strings emitted on each page, in order.
    fn texts_per_page(
        pages: &[crate::render::layout::draw_command::LayoutedPage],
    ) -> Vec<Vec<String>> {
        pages
            .iter()
            .map(|p| {
                p.commands
                    .iter()
                    .filter_map(|c| match c {
                        DrawCommand::Text { text, .. } => Some(text.to_string()),
                        _ => None,
                    })
                    .collect()
            })
            .collect()
    }

    /// A floating table whose laid-out height exceeds the available
    /// space on its anchor page must split at row boundaries. With the
    /// 100pt-tall small_config and an anchor at y=15, 8 rows of ~14pt
    /// (~112pt total) cannot fit in the 85pt below the anchor — at
    /// least one row must spill to page 2.
    #[test]
    fn floating_table_splits_when_taller_than_anchor_page() {
        let blocks = vec![floating_table_with_rows(8, 15.0)];
        let pages = layout_section(
            &blocks,
            &small_config(),
            None,
            Pt::ZERO,
            Pt::new(14.0),
            None,
        );
        assert!(
            pages.len() >= 2,
            "overflowing floating table must paginate: got {} pages",
            pages.len()
        );

        // The 8 row labels must appear collectively across pages.
        let per_page = texts_per_page(&pages);
        let all: Vec<&String> = per_page.iter().flatten().collect();
        for i in 0..8 {
            let needle = format!("r{i}");
            assert!(
                all.iter().any(|t| t.as_str() == needle),
                "row {needle} missing from output entirely",
            );
        }
    }

    /// Rows in a paginated floating table do not duplicate: each row
    /// appears on exactly one page. (This is what fails in the bug —
    /// the current code emits every row on the anchor page, then
    /// later content also lands on the anchor page, producing visual
    /// overdraw.)
    #[test]
    fn floating_table_split_rows_do_not_overlap_across_pages() {
        let blocks = vec![floating_table_with_rows(8, 15.0)];
        let pages = layout_section(
            &blocks,
            &small_config(),
            None,
            Pt::ZERO,
            Pt::new(14.0),
            None,
        );
        let per_page = texts_per_page(&pages);
        for i in 0..8 {
            let needle = format!("r{i}");
            let on_pages: Vec<usize> = per_page
                .iter()
                .enumerate()
                .filter_map(|(idx, page)| {
                    if page.iter().any(|t| t == &needle) {
                        Some(idx)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(
                on_pages.len(),
                1,
                "row {needle} appeared on pages {on_pages:?}; floating-table rows must not be duplicated",
            );
        }
    }

    /// Build a floating table with `n` rows, anchored to the page at
    /// `y_offset`, with an explicit `tblOverlap` value.
    fn floating_table_with_rows_and_overlap(
        n: usize,
        y_offset: f32,
        overlap: Option<crate::model::TableOverlap>,
    ) -> LayoutBlock {
        LayoutBlock::Table {
            rows: (0..n).map(|i| row_with_label(&format!("r{i}"))).collect(),
            col_widths: vec![Pt::new(100.0)],
            border_config: None,
            indent: Pt::ZERO,
            alignment: None,
            float_info: Some(super::TableFloatInfo {
                right_gap: Pt::ZERO,
                bottom_gap: Pt::ZERO,
                x_align: None,
                y_offset: Pt::new(y_offset),
                vert_anchor: crate::model::TableAnchor::Page,
                overlap,
            }),
            style_id: None,
        }
    }

    /// §17.4.39: two floating tables both declaring `tblOverlap=Never`
    /// must not draw at overlapping y-positions on the same page. The
    /// second table either slides down past the first or spills to
    /// the next page.
    ///
    /// Setup: page 100pt tall, both tables anchored to the same y=15.
    /// First table (3 rows) fits at y=15; second table (3 rows) with
    /// Never must NOT draw on top of the first.
    #[test]
    fn floating_tables_never_overlap_when_both_set_never() {
        use crate::model::TableOverlap;
        let blocks = vec![
            floating_table_with_rows_and_overlap(3, 15.0, Some(TableOverlap::Never)),
            floating_table_with_rows_and_overlap(3, 15.0, Some(TableOverlap::Never)),
        ];
        let pages = layout_section(
            &blocks,
            &small_config(),
            None,
            Pt::ZERO,
            Pt::new(14.0),
            None,
        );

        // Collect (page_idx, x, y, text) for all rows.
        let mut row_positions: Vec<(usize, f32, f32, String)> = Vec::new();
        for (pi, page) in pages.iter().enumerate() {
            for cmd in &page.commands {
                if let DrawCommand::Text { text, position, .. } = cmd {
                    if text.starts_with('r') {
                        row_positions.push((
                            pi,
                            position.x.raw(),
                            position.y.raw(),
                            text.to_string(),
                        ));
                    }
                }
            }
        }
        // Group by page+x bucket; check no two row texts share the same y.
        // (The bug emits both tables at y=15, so r0/r1/r2 from each table
        // would land on identical y values.)
        for i in 0..row_positions.len() {
            for j in (i + 1)..row_positions.len() {
                let (pi, xi, yi, ti) = &row_positions[i];
                let (pj, xj, yj, tj) = &row_positions[j];
                if pi == pj && (xi - xj).abs() < 1.0 && (yi - yj).abs() < 0.5 {
                    panic!(
                        "two row labels at same position on page {pi}: {ti:?} and {tj:?} both at y={yi}",
                    );
                }
            }
        }
    }

    /// §17.4.39 default behavior (overlap omitted) — overlap is
    /// permitted. Two tables at the same anchor on the same page
    /// DO draw at overlapping y-positions. This is intentional.
    #[test]
    fn floating_tables_overlap_by_default() {
        let blocks = vec![
            floating_table_with_rows_and_overlap(3, 15.0, None),
            floating_table_with_rows_and_overlap(3, 15.0, None),
        ];
        let pages = layout_section(
            &blocks,
            &small_config(),
            None,
            Pt::ZERO,
            Pt::new(14.0),
            None,
        );
        // Both tables drawn on page 1; the test verifies that we
        // don't accidentally shift when overlap is permitted.
        assert!(!pages.is_empty());
        let page1_rows: Vec<f32> = pages[0]
            .commands
            .iter()
            .filter_map(|c| match c {
                DrawCommand::Text { text, position, .. } if text.starts_with('r') => {
                    Some(position.y.raw())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            page1_rows.len(),
            6,
            "with overlap permitted, both tables draw on page 1: got {} row entries",
            page1_rows.len()
        );
    }

    /// The continuation slice starts at the top content area on page 2
    /// (margin.top), not at the original `tblpY` anchor. §17.4.59 only
    /// anchors the first slice; the continuation flows at the top of
    /// each subsequent page. Behavioral check: the first text on page
    /// 2 must sit *higher* on the page than the first text on page 1
    /// (lower y), because page 1 starts at the anchor (>= margin.top)
    /// and page 2 starts at margin.top exactly.
    #[test]
    fn floating_table_continuation_anchors_at_top_margin() {
        let blocks = vec![floating_table_with_rows(8, 15.0)];
        let config = small_config();
        let pages = layout_section(&blocks, &config, None, Pt::ZERO, Pt::new(14.0), None);
        assert!(pages.len() >= 2, "test precondition: expected overflow");

        let first_text_y =
            |page: &crate::render::layout::draw_command::LayoutedPage| -> Option<f32> {
                page.commands.iter().find_map(|c| match c {
                    DrawCommand::Text { position, .. } => Some(position.y.raw()),
                    _ => None,
                })
            };
        let y_p1 = first_text_y(&pages[0]).expect("page 1 must contain table content");
        let y_p2 = first_text_y(&pages[1]).expect("page 2 must contain table content");
        let anchor_y = 15.0;
        let margin_top = config.margins.top.raw();
        // Anchor sits at y=15 (> margin.top=10), so the page-1 baseline
        // is offset by 5pt versus page 2's continuation baseline.
        assert!(
            y_p2 < y_p1,
            "continuation (page 2 first y={y_p2}) must start above the anchor (page 1 first y={y_p1}); anchor={anchor_y}, margin.top={margin_top}",
        );
        // Sanity: page 2 first text is within the content area.
        assert!(y_p2 >= margin_top - 1.0);
    }

    // ── §17.3.1.24 paragraph border grouping tests ─────────────────────

    #[test]
    fn identical_borders_suppress_second_top() {
        use crate::render::layout::paragraph::{BorderLine, ParagraphBorderStyle};
        let border = Some(ParagraphBorderStyle {
            top: Some(BorderLine {
                width: Pt::new(0.5),
                color: RgbColor::BLACK,
                space: Pt::new(1.0),
            }),
            bottom: None,
            left: None,
            right: None,
        });
        let style1 = ParagraphStyle {
            borders: border.clone(),
            ..Default::default()
        };
        let style2 = ParagraphStyle {
            borders: border,
            ..Default::default()
        };

        let blocks = vec![
            LayoutBlock::Paragraph {
                fragments: vec![text_frag("para1", 30.0, 14.0)],
                style: style1,
                page_break_before: false,
                footnotes: vec![],
                floating_images: vec![],
                floating_shapes: vec![],
            },
            LayoutBlock::Paragraph {
                fragments: vec![text_frag("para2", 30.0, 14.0)],
                style: style2,
                page_break_before: false,
                footnotes: vec![],
                floating_images: vec![],
                floating_shapes: vec![],
            },
        ];
        let pages = layout_section(
            &blocks,
            &small_config(),
            None,
            Pt::ZERO,
            Pt::new(14.0),
            None,
        );

        // Count Line draw commands (border lines).
        // Only the first paragraph should draw its top border; the second's
        // top border is suppressed by §17.3.1.24 grouping.
        let line_cmds: Vec<_> = pages[0]
            .commands
            .iter()
            .filter(|c| matches!(c, DrawCommand::Line { .. }))
            .collect();
        assert_eq!(
            line_cmds.len(),
            1,
            "only one top border line (grouped): got {}",
            line_cmds.len()
        );
    }
}
