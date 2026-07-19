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
pub(crate) use layout::layout_section_with_clearance;
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
    use crate::render::layout::header_footer::HeaderFooterClearance;
    use crate::render::layout::page::PageConfig;
    use crate::render::layout::paragraph::ParagraphStyle;
    use crate::render::layout::table::{
        TableBorderConfig, TableBorderLine, TableBorderStyle, TableCellInput, TableRowInput,
    };
    use crate::render::resolve::color::RgbColor;
    use crate::render::resolve::header_footer::HeaderFooterSet;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::{Mutex, Once};

    struct TableWarningLogger;

    thread_local! {
        static CAPTURE_TABLE_WARNINGS: Cell<bool> = const { Cell::new(false) };
    }

    static TABLE_WARNING_LOGGER: TableWarningLogger = TableWarningLogger;
    static TABLE_WARNING_LOGGER_INIT: Once = Once::new();
    static TABLE_WARNINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

    impl log::Log for TableWarningLogger {
        fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
            metadata.level() <= log::Level::Warn
        }

        fn log(&self, record: &log::Record<'_>) {
            if self.enabled(record.metadata())
                && record.target().starts_with("dxpdf::render::layout::table")
                && CAPTURE_TABLE_WARNINGS.with(Cell::get)
            {
                TABLE_WARNINGS
                    .lock()
                    .unwrap()
                    .push(record.args().to_string());
            }
        }

        fn flush(&self) {}
    }

    fn capture_table_warnings<T>(run: impl FnOnce() -> T) -> (T, Vec<String>) {
        TABLE_WARNING_LOGGER_INIT.call_once(|| {
            log::set_logger(&TABLE_WARNING_LOGGER).unwrap();
            log::set_max_level(log::LevelFilter::Warn);
        });
        TABLE_WARNINGS.lock().unwrap().clear();
        CAPTURE_TABLE_WARNINGS.with(|capture| capture.set(true));
        let result = run();
        CAPTURE_TABLE_WARNINGS.with(|capture| capture.set(false));
        let warnings = std::mem::take(&mut *TABLE_WARNINGS.lock().unwrap());
        (result, warnings)
    }

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

    fn styled_para_block(text: &str, keep_next: bool, page_break_before: bool) -> LayoutBlock {
        LayoutBlock::Paragraph {
            fragments: vec![text_frag(text, 30.0, 14.0)],
            style: ParagraphStyle {
                keep_next,
                ..Default::default()
            },
            page_break_before,
            footnotes: vec![],
            floating_images: vec![],
            floating_shapes: vec![],
        }
    }

    fn one_column_table(rows: &[(&str, bool)]) -> LayoutBlock {
        LayoutBlock::Table {
            rows: rows
                .iter()
                .map(|(text, header)| TableRowInput {
                    cells: vec![TableCellInput {
                        blocks: vec![para_block(text, 30.0)],
                        margins: PtEdgeInsets::ZERO,
                        grid_span: 1,
                        shading: None,
                        cell_borders: None,
                        vertical_merge: None,
                        vertical_align: crate::render::layout::table::CellVAlign::Top,
                    }],
                    height_rule: None,
                    is_header: Some(*header),
                    cant_split: Some(true),
                    grid_before: 0,
                    grid_after: 0,
                    border_overrides: None,
                })
                .collect(),
            col_widths: vec![Pt::new(100.0)],
            border_config: None,
            indent: Pt::ZERO,
            alignment: None,
            float_info: None,
            style_id: None,
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
    fn each_page_uses_its_selected_header_and_footer_clearance() {
        let config = small_config();
        let clearance = HeaderFooterClearance::new(
            &config,
            HeaderFooterSet {
                default: None,
                first: Some(Pt::new(30.0)),
                even: None,
            },
            HeaderFooterSet {
                default: None,
                first: Some(Pt::new(30.0)),
                even: None,
            },
            true,
            false,
            1,
        );
        let blocks: Vec<_> = (0..5).map(|i| para_block(&format!("p{i}"), 30.0)).collect();

        let pages = layout_section_with_clearance(
            &blocks,
            &config,
            None,
            Pt::ZERO,
            Pt::new(14.0),
            None,
            &clearance,
        );

        assert_eq!(pages.len(), 2, "page 2 must use the shorter default slots");
        let page_texts = pages
            .iter()
            .map(|page| {
                page.commands
                    .iter()
                    .filter_map(|command| match command {
                        DrawCommand::Text { text, position, .. } => {
                            Some((text.to_string(), position.y))
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            page_texts[0].len(),
            2,
            "40pt first-page body fits two lines"
        );
        assert_eq!(
            page_texts[1].len(),
            3,
            "80pt default-page body fits the remainder"
        );
        assert!(
            page_texts[1][0].1 < page_texts[0][0].1,
            "page 2 body must start at the shorter default-header boundary",
        );
    }

    #[test]
    fn paragraph_moved_to_taller_page_is_relaid_before_following_block() {
        let config = small_config();
        let clearance = HeaderFooterClearance::new(
            &config,
            HeaderFooterSet {
                default: None,
                first: Some(Pt::new(30.0)),
                even: None,
            },
            HeaderFooterSet {
                default: None,
                first: Some(Pt::new(30.0)),
                even: None,
            },
            true,
            false,
            1,
        );
        let moved = LayoutBlock::Paragraph {
            fragments: (0..4)
                .map(|i| text_frag(&format!("moved-{i} "), 170.0, 14.0))
                .collect(),
            style: ParagraphStyle::default(),
            page_break_before: false,
            footnotes: vec![],
            floating_images: vec![],
            floating_shapes: vec![],
        };
        let pages = layout_section_with_clearance(
            &[para_block("before", 30.0), moved, para_block("after", 30.0)],
            &config,
            None,
            Pt::ZERO,
            Pt::new(14.0),
            None,
            &clearance,
        );

        assert_eq!(pages.len(), 2);
        let moved_last_y = pages[1]
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, position, .. } if text.starts_with("moved-") => {
                    Some(position.y.raw())
                }
                _ => None,
            })
            .max_by(f32::total_cmp)
            .expect("moved paragraph text");
        let after_y = pages[1]
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Text { text, position, .. } if text.as_ref() == "after" => {
                    Some(position.y.raw())
                }
                _ => None,
            })
            .expect("following paragraph text");

        assert!(
            after_y > moved_last_y,
            "following paragraph at {after_y:?} overlaps moved paragraph ending at {moved_last_y:?}",
        );
    }

    #[test]
    fn footnotes_render_from_the_selected_footer_boundary_once() {
        let config = small_config();
        let clearance = HeaderFooterClearance::new(
            &config,
            HeaderFooterSet::default(),
            HeaderFooterSet {
                default: Some(Pt::new(30.0)),
                first: None,
                even: None,
            },
            false,
            false,
            1,
        );
        let block = LayoutBlock::Paragraph {
            fragments: vec![text_frag("body", 30.0, 14.0)],
            style: ParagraphStyle::default(),
            page_break_before: false,
            footnotes: vec![(
                vec![text_frag("footnote", 30.0, 14.0)],
                ParagraphStyle::default(),
            )],
            floating_images: vec![],
            floating_shapes: vec![],
        };

        let pages = layout_section_with_clearance(
            &[block],
            &config,
            None,
            Pt::ZERO,
            Pt::new(14.0),
            None,
            &clearance,
        );
        let separator_y = pages[0]
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Line { line, .. } => Some(line.start.y),
                _ => None,
            })
            .expect("footnote separator");

        assert_eq!(separator_y, Pt::new(52.0));
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

    #[test]
    fn keep_next_chain_moves_with_terminal_paragraph() {
        let mut blocks: Vec<_> = (0..3)
            .map(|i| para_block(&format!("fill{i}"), 30.0))
            .collect();
        blocks.extend([
            styled_para_block("heading", true, false),
            styled_para_block("subheading", true, false),
            styled_para_block("body", false, false),
        ]);
        let pages = layout_section(
            &blocks,
            &small_config(),
            None,
            Pt::ZERO,
            Pt::new(14.0),
            None,
        );
        let per_page = texts_per_page(&pages);
        let last = per_page.last().unwrap();
        assert!(last.iter().any(|text| text == "heading"));
        assert!(last.iter().any(|text| text == "subheading"));
        assert!(last.iter().any(|text| text == "body"));
    }

    #[test]
    fn contextual_keep_next_chain_stays_when_collapsed_spacing_fits() {
        let mut blocks: Vec<_> = (0..2)
            .map(|i| para_block(&format!("fill{i}"), 30.0))
            .collect();
        let style = ParagraphStyle {
            keep_next: true,
            contextual_spacing: true,
            style_id: Some(crate::model::StyleId::new("contextual")),
            space_before: Pt::new(10.0),
            space_after: Pt::new(10.0),
            ..Default::default()
        };
        blocks.extend([
            LayoutBlock::Paragraph {
                fragments: vec![text_frag("heading", 30.0, 14.0)],
                style: style.clone(),
                page_break_before: false,
                footnotes: vec![],
                floating_images: vec![],
                floating_shapes: vec![],
            },
            LayoutBlock::Paragraph {
                fragments: vec![text_frag("body", 30.0, 14.0)],
                style: ParagraphStyle {
                    keep_next: false,
                    ..style
                },
                page_break_before: false,
                footnotes: vec![],
                floating_images: vec![],
                floating_shapes: vec![],
            },
        ]);

        let pages = layout_section(
            &blocks,
            &small_config(),
            None,
            Pt::ZERO,
            Pt::new(14.0),
            None,
        );

        assert_eq!(pages.len(), 1);
        let texts = &texts_per_page(&pages)[0];
        assert!(texts.iter().any(|text| text == "heading"));
        assert!(texts.iter().any(|text| text == "body"));
    }

    #[test]
    fn contextual_predecessor_allows_keep_next_table_chain_on_fresh_page() {
        let style = ParagraphStyle {
            keep_next: true,
            contextual_spacing: true,
            style_id: Some(crate::model::StyleId::new("contextual")),
            space_before: Pt::new(20.0),
            space_after: Pt::ZERO,
            ..Default::default()
        };
        let mut blocks = vec![
            para_block("fill", 30.0),
            LayoutBlock::Paragraph {
                fragments: vec![text_frag("predecessor", 30.0, 14.0)],
                style: ParagraphStyle {
                    keep_next: false,
                    ..style.clone()
                },
                page_break_before: false,
                footnotes: vec![],
                floating_images: vec![],
                floating_shapes: vec![],
            },
        ];
        blocks.extend((0..4).map(|index| LayoutBlock::Paragraph {
            fragments: vec![text_frag(&format!("chain{index}"), 30.0, 14.0)],
            style: style.clone(),
            page_break_before: false,
            footnotes: vec![],
            floating_images: vec![],
            floating_shapes: vec![],
        }));
        blocks.push(one_column_table(&[("row0", false), ("row1", false)]));

        let pages = layout_section(
            &blocks,
            &small_config(),
            None,
            Pt::ZERO,
            Pt::new(14.0),
            None,
        );
        let per_page = texts_per_page(&pages);
        let heading_page = per_page
            .iter()
            .position(|page| page.iter().any(|text| text == "chain0"))
            .unwrap();
        let row0_page = per_page
            .iter()
            .position(|page| page.iter().any(|text| text == "row0"))
            .unwrap();

        assert_eq!(heading_page, row0_page);
    }

    #[test]
    fn keep_next_heading_moves_with_first_table_row() {
        for header in [false, true] {
            let mut blocks: Vec<_> = (0..4)
                .map(|i| para_block(&format!("fill{i}"), 30.0))
                .collect();
            blocks.push(styled_para_block("heading", true, false));
            blocks.push(one_column_table(&[
                ("row0", header),
                ("row1", false),
                ("row2", false),
            ]));
            let pages = layout_section(
                &blocks,
                &small_config(),
                None,
                Pt::ZERO,
                Pt::new(14.0),
                None,
            );
            let per_page = texts_per_page(&pages);
            let heading_page = per_page
                .iter()
                .position(|p| p.iter().any(|t| t == "heading"))
                .unwrap();
            let row_page = per_page
                .iter()
                .position(|p| p.iter().any(|t| t == "row0"))
                .unwrap();
            assert_eq!(heading_page, row_page);
        }
    }

    #[test]
    fn keep_next_heading_moves_with_bordered_first_table_row() {
        let mut blocks: Vec<_> = (0..3)
            .map(|i| para_block(&format!("fill{i}"), 30.0))
            .collect();
        blocks.push(styled_para_block("heading", true, false));
        let mut table = one_column_table(&[
            ("row0", false),
            ("row1", false),
            ("row2", false),
            ("row3", false),
        ]);
        if let LayoutBlock::Table { border_config, .. } = &mut table {
            *border_config = Some(TableBorderConfig {
                top: None,
                bottom: None,
                left: None,
                right: None,
                inside_h: Some(TableBorderLine {
                    width: Pt::new(12.0),
                    color: RgbColor::BLACK,
                    style: TableBorderStyle::Single,
                }),
                inside_v: None,
            });
        }
        blocks.push(table);

        let pages = layout_section(
            &blocks,
            &small_config(),
            None,
            Pt::ZERO,
            Pt::new(14.0),
            None,
        );
        let per_page = texts_per_page(&pages);
        let heading_page = per_page
            .iter()
            .position(|p| p.iter().any(|text| text == "heading"))
            .unwrap();
        let row0_page = per_page
            .iter()
            .position(|p| p.iter().any(|text| text == "row0"))
            .unwrap();

        assert_eq!(heading_page, row0_page);
        assert!(per_page.iter().skip(row0_page + 1).any(|page| page
            .iter()
            .any(|text| text == "row1" || text == "row2" || text == "row3")));
    }

    #[test]
    fn oversized_splittable_first_table_row_does_not_move_heading() {
        let mut blocks: Vec<_> = (0..4)
            .map(|i| para_block(&format!("fill{i}"), 30.0))
            .collect();
        blocks.push(styled_para_block("heading", true, false));
        blocks.push(LayoutBlock::Table {
            rows: vec![TableRowInput {
                cells: vec![TableCellInput {
                    blocks: vec![LayoutBlock::Paragraph {
                        fragments: (0..12)
                            .flat_map(|i| {
                                [
                                    text_frag(&format!("row0-{i}"), 30.0, 14.0),
                                    Fragment::LineBreak {
                                        line_height: Pt::new(14.0),
                                    },
                                ]
                            })
                            .collect(),
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
        });

        let pages = layout_section(
            &blocks,
            &small_config(),
            None,
            Pt::ZERO,
            Pt::new(14.0),
            None,
        );
        let per_page = texts_per_page(&pages);
        let heading_page = per_page
            .iter()
            .position(|page| page.iter().any(|text| text == "heading"))
            .unwrap();
        let first_row_page = per_page
            .iter()
            .position(|page| page.iter().any(|text| text == "row0-0"))
            .unwrap();

        assert!(heading_page < first_row_page);
        let flattened: Vec<_> = per_page.into_iter().flatten().collect();
        for index in 0..12 {
            let needle = format!("row0-{index}");
            assert_eq!(flattened.iter().filter(|text| *text == &needle).count(), 1);
        }
    }

    #[test]
    fn keep_next_heading_stays_before_oversized_leading_vmerge_group() {
        let mut blocks: Vec<_> = (0..4)
            .map(|i| para_block(&format!("fill{i}"), 30.0))
            .collect();
        blocks.push(styled_para_block("heading", true, false));
        blocks.push(LayoutBlock::Table {
            rows: vec![
                TableRowInput {
                    cells: vec![
                        TableCellInput {
                            blocks: (0..6)
                                .map(|i| para_block(&format!("restart-{i}"), 30.0))
                                .collect(),
                            margins: PtEdgeInsets::ZERO,
                            grid_span: 1,
                            shading: None,
                            cell_borders: None,
                            vertical_merge: Some(
                                crate::render::layout::table::VerticalMergeState::Restart,
                            ),
                            vertical_align: crate::render::layout::table::CellVAlign::Top,
                        },
                        TableCellInput {
                            blocks: vec![para_block("row0-peer", 30.0)],
                            margins: PtEdgeInsets::ZERO,
                            grid_span: 1,
                            shading: None,
                            cell_borders: None,
                            vertical_merge: None,
                            vertical_align: crate::render::layout::table::CellVAlign::Top,
                        },
                    ],
                    height_rule: None,
                    is_header: None,
                    cant_split: None,
                    grid_before: 0,
                    grid_after: 0,
                    border_overrides: None,
                },
                TableRowInput {
                    cells: vec![
                        TableCellInput {
                            blocks: vec![],
                            margins: PtEdgeInsets::ZERO,
                            grid_span: 1,
                            shading: None,
                            cell_borders: None,
                            vertical_merge: Some(
                                crate::render::layout::table::VerticalMergeState::Continue,
                            ),
                            vertical_align: crate::render::layout::table::CellVAlign::Top,
                        },
                        TableCellInput {
                            blocks: (0..2)
                                .map(|i| para_block(&format!("continuation-{i}"), 30.0))
                                .collect(),
                            margins: PtEdgeInsets::ZERO,
                            grid_span: 1,
                            shading: None,
                            cell_borders: None,
                            vertical_merge: None,
                            vertical_align: crate::render::layout::table::CellVAlign::Top,
                        },
                    ],
                    height_rule: None,
                    is_header: None,
                    cant_split: None,
                    grid_before: 0,
                    grid_after: 0,
                    border_overrides: None,
                },
                TableRowInput {
                    cells: vec![
                        TableCellInput {
                            blocks: vec![para_block("after-merged", 30.0)],
                            margins: PtEdgeInsets::ZERO,
                            grid_span: 1,
                            shading: None,
                            cell_borders: None,
                            vertical_merge: None,
                            vertical_align: crate::render::layout::table::CellVAlign::Top,
                        },
                        TableCellInput {
                            blocks: vec![para_block("after-peer", 30.0)],
                            margins: PtEdgeInsets::ZERO,
                            grid_span: 1,
                            shading: None,
                            cell_borders: None,
                            vertical_merge: None,
                            vertical_align: crate::render::layout::table::CellVAlign::Top,
                        },
                    ],
                    height_rule: None,
                    is_header: None,
                    cant_split: None,
                    grid_before: 0,
                    grid_after: 0,
                    border_overrides: None,
                },
            ],
            col_widths: vec![Pt::new(50.0), Pt::new(50.0)],
            border_config: None,
            indent: Pt::ZERO,
            alignment: None,
            float_info: None,
            style_id: None,
        });

        let pages = layout_section(
            &blocks,
            &small_config(),
            None,
            Pt::ZERO,
            Pt::new(14.0),
            None,
        );
        let per_page = texts_per_page(&pages);
        let heading_page = per_page
            .iter()
            .position(|page| page.iter().any(|text| text == "heading"))
            .unwrap();
        let restart_page = per_page
            .iter()
            .position(|page| page.iter().any(|text| text == "restart-0"))
            .unwrap();

        assert_eq!(heading_page, 0);
        assert!(heading_page < restart_page);
        let flattened: Vec<_> = per_page.into_iter().flatten().collect();
        for text in [
            "restart-0",
            "restart-1",
            "restart-2",
            "restart-3",
            "restart-4",
            "restart-5",
            "row0-peer",
            "continuation-0",
            "continuation-1",
            "after-merged",
            "after-peer",
        ] {
            assert_eq!(
                flattened
                    .iter()
                    .filter(|candidate| *candidate == text)
                    .count(),
                1
            );
        }
    }

    #[test]
    fn large_table_keep_next_preflight_does_not_probe_later_groups() {
        let mut blocks: Vec<_> = (0..4)
            .map(|i| para_block(&format!("fill{i}"), 30.0))
            .collect();
        blocks.push(styled_para_block("heading", true, false));
        blocks.push(LayoutBlock::Table {
            rows: (0..24)
                .map(|i| row_with_label(&format!("row{i}")))
                .collect(),
            col_widths: vec![Pt::new(100.0)],
            border_config: None,
            indent: Pt::ZERO,
            alignment: None,
            float_info: None,
            style_id: None,
        });

        let (pages, warnings) = capture_table_warnings(|| {
            layout_section(
                &blocks,
                &small_config(),
                None,
                Pt::ZERO,
                Pt::new(14.0),
                None,
            )
        });

        assert!(
            warnings.is_empty(),
            "unexpected table warnings: {warnings:?}"
        );
        let flattened: Vec<_> = texts_per_page(&pages).into_iter().flatten().collect();
        assert_eq!(
            flattened.iter().filter(|text| *text == "heading").count(),
            1
        );
        for index in 0..24 {
            let needle = format!("row{index}");
            assert_eq!(flattened.iter().filter(|text| *text == &needle).count(), 1);
        }
    }

    #[test]
    fn keep_next_does_not_force_an_entire_multi_page_table() {
        let blocks = vec![
            styled_para_block("heading", true, false),
            one_column_table(&[
                ("row0", true),
                ("row1", false),
                ("row2", false),
                ("row3", false),
                ("row4", false),
                ("row5", false),
            ]),
        ];
        let pages = layout_section(
            &blocks,
            &small_config(),
            None,
            Pt::ZERO,
            Pt::new(14.0),
            None,
        );
        assert!(pages.len() > 1);
        assert!(texts_per_page(&pages)[0]
            .iter()
            .any(|text| text == "heading"));
    }

    #[test]
    fn explicit_page_break_terminates_keep_next_group() {
        let blocks = vec![
            styled_para_block("heading", true, false),
            styled_para_block("forced", false, true),
        ];
        let pages = layout_section(
            &blocks,
            &small_config(),
            None,
            Pt::ZERO,
            Pt::new(14.0),
            None,
        );
        let per_page = texts_per_page(&pages);
        assert!(per_page[0].iter().any(|text| text == "heading"));
        assert!(per_page[1].iter().any(|text| text == "forced"));
    }

    #[test]
    fn oversized_keep_next_chain_makes_progress_without_duplicates() {
        let mut blocks = Vec::new();
        for index in 0..6 {
            blocks.push(styled_para_block(
                &format!("chain{index}"),
                index < 5,
                false,
            ));
        }
        let pages = layout_section(
            &blocks,
            &small_config(),
            None,
            Pt::ZERO,
            Pt::new(14.0),
            None,
        );
        let flattened: Vec<_> = texts_per_page(&pages).into_iter().flatten().collect();
        for index in 0..6 {
            let needle = format!("chain{index}");
            assert_eq!(flattened.iter().filter(|text| *text == &needle).count(), 1);
        }
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
