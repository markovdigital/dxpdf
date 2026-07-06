//! Paragraph layout — line fitting, alignment, spacing, borders, shading.
//!
//! Implements the LayoutBox protocol: receives BoxConstraints, returns PtSize,
//! emits DrawCommands at absolute offsets during paint.

mod borders;
mod line_emit;
mod types;

pub use types::*;

use super::draw_command::DrawCommand;
use super::fragment::Fragment;
use super::BoxConstraints;
use crate::render::dimension::Pt;
use crate::render::geometry::{PtOffset, PtRect, PtSize};

use borders::emit_paragraph_borders_and_shading;
use line_emit::{
    compute_line_placements, emit_line_commands, resolve_line_height, split_oversized_fragments,
};

// ── Tab leader rendering constants ────────────────────────────────────────────

/// Maximum font size used when measuring/drawing tab leader characters (pt).
/// Caps at 12pt so leaders remain legible regardless of paragraph line height.
const LEADER_FONT_SIZE_CAP: Pt = Pt::new(12.0);

/// Fallback width for a single leader character when no measurer is available (pt).
const LEADER_CHAR_WIDTH_FALLBACK: Pt = Pt::new(4.0);

/// A fitted line together with the per-line float adjustments that were active when it was placed.
struct LinePlacement {
    line: super::line::FittedLine,
    /// Width stolen from the left by an active float.
    float_left: Pt,
    /// Width stolen from the right by an active float.
    float_right: Pt,
}

/// Shared layout parameters threaded through `compute_line_placements` and
/// `emit_line_commands`.
struct LineLayoutParams {
    content_width: Pt,
    /// The full constraint width (page/cell text area between margins), before
    /// paragraph indents are subtracted. §17.3.1.30 position tabs measured
    /// `relativeTo="margin"` reference this, not the indented `content_width`.
    max_width: Pt,
    first_line_adjustment: Pt,
    drop_cap_indent: Pt,
    drop_cap_lines: usize,
    default_line_height: Pt,
}

/// Clip inline images wider than `max_width` to the content box, cropping the
/// horizontal overflow rather than scaling.
///
/// Per ECMA-376 §20.4.2.7, a `wp:inline` object's `wp:extent` is its *final*
/// height and width — an inline object is **not** auto-scaled to fit its
/// container. When it is wider than the containing table cell (§17.4.53: an
/// autofit table at 100% width cannot grow its column), Word draws the image at
/// full size and clips the overflow to the cell boundary.
///
/// The retained slice is the **left** part of the object. An inline object that
/// is already wider than the line is placed at the line's start: justification
/// (§17.3.1.13 `w:jc`) distributes only *spare* width, and an over-wide object
/// leaves none — Word does not centre or right-shift it — so it overflows to
/// the right and the cell clips that side. (Our own line fitter agrees: it
/// clamps the alignment offset to `max(0)`.) We reproduce that by keeping the
/// image's height and scale, setting its displayed width to `max_width`, and
/// cropping the source to the left visible fraction, composed with any existing
/// `a:srcRect` crop — no symmetric/centre crop, which would shift the pixels.
///
/// Returns `None` when nothing is oversized, so the common case borrows the
/// input slice and allocates nothing.
fn clip_oversized_images(fragments: &[Fragment], max_width: Pt) -> Option<Vec<Fragment>> {
    let oversized =
        |f: &Fragment| matches!(f, Fragment::Image { size, .. } if size.width > max_width);
    if max_width <= Pt::ZERO || !fragments.iter().any(oversized) {
        return None;
    }
    Some(
        fragments
            .iter()
            .map(|f| match f {
                Fragment::Image {
                    size,
                    rel_id,
                    image_data,
                    src_rect,
                } if size.width > max_width => {
                    // Fraction of the object's width that survives the clip.
                    let visible = max_width.raw() / size.width.raw();
                    // Start from the existing crop (fraction of the source), or
                    // the whole image, then keep its left `visible` sub-fraction
                    // — same origin, narrower width (crop the right overflow).
                    let base = src_rect.unwrap_or_else(|| {
                        PtRect::from_xywh(Pt::ZERO, Pt::ZERO, Pt::new(1.0), Pt::new(1.0))
                    });
                    let cropped = PtRect::from_xywh(
                        base.origin.x,
                        base.origin.y,
                        Pt::new(base.size.width.raw() * visible),
                        base.size.height,
                    );
                    Fragment::Image {
                        // Height and scale are unchanged (§20.4.2.7); only the
                        // displayed width shrinks, showing the left, un-clipped
                        // part.
                        size: PtSize::new(max_width, size.height),
                        rel_id: rel_id.clone(),
                        image_data: image_data.clone(),
                        src_rect: Some(cropped),
                    }
                }
                other => other.clone(),
            })
            .collect(),
    )
}

/// Lay out a paragraph: fit fragments into lines, apply alignment and spacing.
///
/// Returns draw commands positioned relative to (0, 0). The caller positions
/// the paragraph by adding its offset during the paint phase.
pub fn layout_paragraph(
    fragments: &[Fragment],
    constraints: &BoxConstraints,
    style: &ParagraphStyle,
    default_line_height: Pt,
    measure_text: MeasureTextFn<'_>,
) -> ParagraphLayout {
    // §17.3.1.11: drop cap text frame.
    // Drop mode: body text indented by drop cap position + width + hSpace.
    // Margin mode: drop cap is in the margin, body text is NOT indented.
    let drop_cap_indent = style
        .drop_cap
        .as_ref()
        .filter(|dc| !dc.margin_mode)
        .map(|dc| dc.indent + dc.width + dc.h_space)
        .unwrap_or(Pt::ZERO);
    let drop_cap_lines = style
        .drop_cap
        .as_ref()
        .map(|dc| dc.lines as usize)
        .unwrap_or(0);

    // §17.3.1.24: border space is the distance between the border line and the text.
    // Only the space reduces the text area, not the border line width.
    let border_space_left = style
        .borders
        .as_ref()
        .and_then(|b| b.left.as_ref())
        .map(|b| b.space)
        .unwrap_or(Pt::ZERO);
    let border_space_right = style
        .borders
        .as_ref()
        .and_then(|b| b.right.as_ref())
        .map(|b| b.space)
        .unwrap_or(Pt::ZERO);
    let content_width = constraints.max_width
        - style.indent_left
        - style.indent_right
        - border_space_left
        - border_space_right;
    // §17.3.1.12: first-line indent adjusts the first line's available width.
    // Positive = narrower (indent), negative = wider (hanging indent).
    // Drop cap indent also reduces width for the first N lines.
    let first_line_adjustment = style.indent_first_line + drop_cap_indent;

    // Clip inline images wider than the content box to the cell/page width,
    // cropping the overflow the way Word does (§20.4.2.7: the extent is the
    // object's final size, so it is not scaled to fit — the cell clips it).
    let clipped_frags;
    let fragments: &[Fragment] = match clip_oversized_images(fragments, content_width) {
        Some(v) => {
            clipped_frags = v;
            &clipped_frags
        }
        None => fragments,
    };

    // Split oversized text fragments into per-character fragments so narrow
    // cells get character-level line breaking.
    let min_avail = (content_width - first_line_adjustment).max(Pt::ZERO);
    let split_frags;
    let fragments: &[Fragment] = if min_avail > Pt::ZERO {
        split_frags = split_oversized_fragments(fragments, min_avail, measure_text);
        &split_frags
    } else {
        fragments
    };

    // Per-line float adjustment: fit one line at a time, computing the available
    // width for each line based on its absolute y position on the page.
    // Each line stores its (float_left, float_right) adjustments for rendering.
    let params = LineLayoutParams {
        content_width,
        max_width: constraints.max_width,
        first_line_adjustment,
        drop_cap_indent,
        drop_cap_lines,
        default_line_height,
    };
    let line_placements = compute_line_placements(fragments, style, &params);

    let mut commands = Vec::new();
    let mut cursor_y = style.space_before;

    // §17.3.1.11: compute the drop cap baseline.
    // When frame_height is set (lineRule="exact"), use:
    //   baseline = frame_top + frame_height - descent + position_offset
    // Otherwise fall back to aligning with the Nth body line's baseline.
    let drop_cap_baseline_y = if let Some(ref dc) = style.drop_cap {
        if let Some(fh) = dc.frame_height {
            let baseline = cursor_y + fh + dc.position_offset;
            Some(baseline)
        } else {
            // Fallback: align with Nth body line baseline.
            let n = dc.lines.max(1) as usize;
            let mut y = cursor_y;
            for (i, lp) in line_placements.iter().enumerate().take(n) {
                let natural = if lp.line.height > Pt::ZERO {
                    lp.line.height
                } else {
                    default_line_height
                };
                let text_h = if lp.line.text_height > Pt::ZERO {
                    lp.line.text_height
                } else {
                    default_line_height
                };
                let lh = resolve_line_height(natural, text_h, &style.line_spacing);
                if i == n - 1 {
                    y += lp.line.ascent;
                    break;
                }
                y += lh;
            }
            Some(y)
        }
    } else {
        None
    };

    // Render drop cap at the computed baseline.
    if let (Some(ref dc), Some(baseline_y)) = (&style.drop_cap, drop_cap_baseline_y) {
        // §17.3.1.11: position the drop cap using its own paragraph's indent.
        // Drop mode: at the drop cap paragraph's indent (inside text area).
        // Margin mode: in the page margin, to the left of text.
        let dc_x = if dc.margin_mode {
            dc.indent - dc.width - dc.h_space
        } else {
            dc.indent
        };
        for frag in &dc.fragments {
            if let Fragment::Text {
                text, font, color, ..
            } = frag
            {
                commands.push(DrawCommand::Text {
                    position: PtOffset::new(dc_x, baseline_y),
                    text: text.clone(),
                    font_family: font.family.clone(),
                    char_spacing: font.char_spacing,
                    font_size: font.size,
                    bold: font.bold,
                    italic: font.italic,
                    color: *color,
                    text_scale: font.text_scale,
                });
            }
        }
    }

    emit_line_commands(
        &mut commands,
        &mut cursor_y,
        &line_placements,
        fragments,
        style,
        &params,
        measure_text,
    );

    // §17.3.1.24: paragraph border and shading coordinate system.
    // Borders sit at the paragraph indent edges. The border `space` is the
    // distance between the border line and the text content. Top/bottom
    // border space expands the bordered area vertically.
    cursor_y = emit_paragraph_borders_and_shading(
        &mut commands,
        style,
        constraints,
        cursor_y,
        default_line_height,
        line_placements.is_empty(),
    );

    let total_height = constraints
        .constrain(PtSize::new(constraints.max_width, cursor_y))
        .height;

    ParagraphLayout {
        commands,
        size: PtSize::new(constraints.max_width, total_height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Alignment, PTabAlignment, PTabRelativeTo};
    use crate::render::layout::fragment::{FontProps, TextMetrics};
    use crate::render::resolve::color::RgbColor;
    use std::rc::Rc;

    fn text_frag(text: &str, width: f32) -> Fragment {
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
                ascent: Pt::new(10.0),
                descent: Pt::new(4.0),
                leading: Pt::ZERO,
            },
            hyperlink_url: None,
            shading: None,
            border: None,
            baseline_offset: Pt::ZERO,
            text_offset: Pt::ZERO,
        }
    }

    fn body_constraints(width: f32) -> BoxConstraints {
        BoxConstraints::new(Pt::ZERO, Pt::new(width), Pt::ZERO, Pt::new(1000.0))
    }

    fn image_frag(w: f32, h: f32) -> Fragment {
        Fragment::Image {
            size: PtSize::new(Pt::new(w), Pt::new(h)),
            rel_id: "rId1".to_string(),
            image_data: None,
            src_rect: None,
        }
    }

    fn clipped_src(frag: &Fragment) -> (PtSize, PtRect) {
        match frag {
            Fragment::Image { size, src_rect, .. } => (*size, src_rect.expect("crop applied")),
            other => panic!("expected image, got {other:?}"),
        }
    }

    #[test]
    fn clip_oversized_image_crops_left_keeps_height_and_scale() {
        // 200×100 image in a 100pt content box → keep the LEFT 50% of the
        // width, full height. Height is UNCHANGED (not scaled) — a crop, not a
        // fit — and the crop is left-anchored (origin.x stays 0), so the right
        // overflow is what gets clipped, matching Word.
        let frags = vec![image_frag(200.0, 100.0)];
        let out = clip_oversized_images(&frags, Pt::new(100.0)).expect("clip");
        let (size, crop) = clipped_src(&out[0]);
        assert_eq!(size.width.raw(), 100.0);
        assert_eq!(size.height.raw(), 100.0, "height must NOT change");
        assert!(
            (crop.origin.x.raw() - 0.0).abs() < 1e-4,
            "left-anchored, not centred"
        );
        assert!((crop.size.width.raw() - 0.5).abs() < 1e-4);
        assert!((crop.size.height.raw() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn clip_oversized_image_composes_with_existing_src_rect() {
        // Image already cropped to [0.1, 0.9] (width 0.8); clipping to 50% keeps
        // the left 0.4 of the source, at the existing origin.
        let mut frag = image_frag(200.0, 100.0);
        if let Fragment::Image { src_rect, .. } = &mut frag {
            *src_rect = Some(PtRect::from_xywh(
                Pt::new(0.1),
                Pt::ZERO,
                Pt::new(0.8),
                Pt::new(1.0),
            ));
        }
        let out = clip_oversized_images(&[frag], Pt::new(100.0)).expect("clip");
        let crop = clipped_src(&out[0]).1;
        assert!((crop.origin.x.raw() - 0.1).abs() < 1e-4);
        assert!((crop.size.width.raw() - 0.4).abs() < 1e-4);
    }

    #[test]
    fn clip_oversized_images_leaves_a_fitting_image_untouched() {
        // Narrower than the content box → not oversized → borrow, no allocation.
        let frags = vec![image_frag(163.0, 290.0)];
        assert!(clip_oversized_images(&frags, Pt::new(499.0)).is_none());
    }

    #[test]
    fn clip_oversized_images_ignores_wide_non_image_fragments() {
        // Oversized text is handled by split_oversized_fragments, not here.
        let frags = vec![text_frag("very wide text", 999.0)];
        assert!(clip_oversized_images(&frags, Pt::new(100.0)).is_none());
    }

    #[test]
    fn clip_oversized_images_guards_nonpositive_width() {
        let frags = vec![image_frag(516.0, 290.0)];
        assert!(clip_oversized_images(&frags, Pt::ZERO).is_none());
    }

    #[test]
    fn empty_paragraph_has_default_height() {
        let result = layout_paragraph(
            &[],
            &body_constraints(400.0),
            &ParagraphStyle::default(),
            Pt::new(14.0),
            None,
        );
        assert_eq!(result.size.height.raw(), 14.0, "default line height");
        assert!(result.commands.is_empty());
    }

    #[test]
    fn single_line_produces_text_command() {
        let frags = vec![text_frag("hello", 30.0)];
        let result = layout_paragraph(
            &frags,
            &body_constraints(400.0),
            &ParagraphStyle::default(),
            Pt::new(14.0),
            None,
        );

        assert_eq!(result.commands.len(), 1);
        if let DrawCommand::Text { text, position, .. } = &result.commands[0] {
            assert_eq!(&**text, "hello");
            assert_eq!(position.x.raw(), 0.0); // left aligned, no indent
        }
    }

    #[test]
    fn center_alignment_shifts_text() {
        let frags = vec![text_frag("hi", 20.0)];
        let style = ParagraphStyle {
            alignment: Alignment::Center,
            ..Default::default()
        };
        let result = layout_paragraph(
            &frags,
            &body_constraints(100.0),
            &style,
            Pt::new(14.0),
            None,
        );

        if let DrawCommand::Text { position, .. } = &result.commands[0] {
            assert_eq!(position.x.raw(), 40.0); // (100 - 20) / 2
        }
    }

    #[test]
    fn end_alignment_right_aligns() {
        let frags = vec![text_frag("hi", 20.0)];
        let style = ParagraphStyle {
            alignment: Alignment::End,
            ..Default::default()
        };
        let result = layout_paragraph(
            &frags,
            &body_constraints(100.0),
            &style,
            Pt::new(14.0),
            None,
        );

        if let DrawCommand::Text { position, .. } = &result.commands[0] {
            assert_eq!(position.x.raw(), 80.0); // 100 - 20
        }
    }

    #[test]
    fn indentation_shifts_text() {
        let frags = vec![text_frag("text", 40.0)];
        let style = ParagraphStyle {
            indent_left: Pt::new(36.0),
            ..Default::default()
        };
        let result = layout_paragraph(
            &frags,
            &body_constraints(400.0),
            &style,
            Pt::new(14.0),
            None,
        );

        if let DrawCommand::Text { position, .. } = &result.commands[0] {
            assert_eq!(position.x.raw(), 36.0);
        }
    }

    #[test]
    fn first_line_indent() {
        let frags = vec![text_frag("first ", 40.0), text_frag("second", 40.0)];
        let style = ParagraphStyle {
            indent_first_line: Pt::new(24.0),
            ..Default::default()
        };
        let result = layout_paragraph(
            &frags,
            &body_constraints(400.0),
            &style,
            Pt::new(14.0),
            None,
        );

        if let DrawCommand::Text { position, .. } = &result.commands[0] {
            assert_eq!(position.x.raw(), 24.0, "first line indented");
        }
    }

    #[test]
    fn space_before_and_after() {
        let frags = vec![text_frag("text", 30.0)];
        let style = ParagraphStyle {
            space_before: Pt::new(10.0),
            space_after: Pt::new(8.0),
            ..Default::default()
        };
        let result = layout_paragraph(
            &frags,
            &body_constraints(400.0),
            &style,
            Pt::new(14.0),
            None,
        );

        // Height should be: space_before(10) + line_height(14) + space_after(8) = 32
        assert_eq!(result.size.height.raw(), 32.0);

        // Text y should include space_before
        if let DrawCommand::Text { position, .. } = &result.commands[0] {
            assert!(
                position.y.raw() >= 10.0,
                "y should account for space_before"
            );
        }
    }

    #[test]
    fn line_spacing_exact() {
        let frags = vec![text_frag("line1 ", 60.0), text_frag("line2", 60.0)];
        let style = ParagraphStyle {
            line_spacing: LineSpacingRule::Exact(Pt::new(20.0)),
            ..Default::default()
        };
        // With max_width=80, they'll break into 2 lines
        let result = layout_paragraph(&frags, &body_constraints(80.0), &style, Pt::new(14.0), None);

        assert_eq!(result.size.height.raw(), 40.0, "2 lines * 20pt each");
    }

    #[test]
    fn line_spacing_at_least_with_larger_natural() {
        let frags = vec![text_frag("text", 30.0)];
        let style = ParagraphStyle {
            line_spacing: LineSpacingRule::AtLeast(Pt::new(10.0)),
            ..Default::default()
        };
        let result = layout_paragraph(
            &frags,
            &body_constraints(400.0),
            &style,
            Pt::new(14.0),
            None,
        );

        // Natural height is 14, at-least is 10 → should be 14
        assert_eq!(result.size.height.raw(), 14.0);
    }

    #[test]
    fn wrapping_produces_multiple_lines() {
        let frags = vec![
            text_frag("word1 ", 45.0),
            text_frag("word2 ", 45.0),
            text_frag("word3", 45.0),
        ];
        let result = layout_paragraph(
            &frags,
            &body_constraints(80.0),
            &ParagraphStyle::default(),
            Pt::new(14.0),
            None,
        );

        // Should have 3 text commands (one per word, each on its own line)
        let text_count = result
            .commands
            .iter()
            .filter(|c| matches!(c, DrawCommand::Text { .. }))
            .count();
        assert_eq!(text_count, 3);
        // Height: 3 lines * 14pt = 42pt
        assert_eq!(result.size.height.raw(), 42.0);
    }

    #[test]
    fn resolve_line_height_auto_text_only() {
        // Text-only line: multiplier applies to text_height.
        assert_eq!(
            resolve_line_height(Pt::new(14.0), Pt::new(14.0), &LineSpacingRule::Auto(1.0)).raw(),
            14.0
        );
        assert_eq!(
            resolve_line_height(Pt::new(14.0), Pt::new(14.0), &LineSpacingRule::Auto(1.5)).raw(),
            21.0
        );
    }

    #[test]
    fn resolve_line_height_auto_image_line() {
        // Image-only line: natural=325 (image), text_height=0 (no text).
        // The multiplier does NOT inflate the image height.
        let h = resolve_line_height(Pt::new(325.0), Pt::ZERO, &LineSpacingRule::Auto(1.08));
        assert_eq!(h.raw(), 325.0, "image height should not be multiplied");
    }

    #[test]
    fn resolve_line_height_auto_mixed_line() {
        // Line with text (14pt) and image (100pt): multiplier scales text only.
        // max(14*1.5=21, 100) = 100.
        let h = resolve_line_height(Pt::new(100.0), Pt::new(14.0), &LineSpacingRule::Auto(1.5));
        assert_eq!(h.raw(), 100.0, "image dominates");
    }

    #[test]
    fn resolve_line_height_exact_overrides() {
        assert_eq!(
            resolve_line_height(
                Pt::new(14.0),
                Pt::new(14.0),
                &LineSpacingRule::Exact(Pt::new(20.0))
            )
            .raw(),
            20.0
        );
    }

    #[test]
    fn resolve_line_height_at_least() {
        assert_eq!(
            resolve_line_height(
                Pt::new(14.0),
                Pt::new(14.0),
                &LineSpacingRule::AtLeast(Pt::new(10.0))
            )
            .raw(),
            14.0,
            "natural > minimum"
        );
        assert_eq!(
            resolve_line_height(
                Pt::new(8.0),
                Pt::new(8.0),
                &LineSpacingRule::AtLeast(Pt::new(10.0))
            )
            .raw(),
            10.0,
            "minimum > natural"
        );
    }

    // ── Absolute position tabs (§17.3.1.30) ──

    fn ptab_frag(align: PTabAlignment, relative_to: PTabRelativeTo) -> Fragment {
        Fragment::PTab {
            align,
            relative_to,
            leader: crate::model::TabLeader::None,
            line_height: Pt::new(12.0),
        }
    }

    /// Return the x positions of every emitted Text command, in order.
    fn text_xs(result: &ParagraphLayout) -> Vec<f32> {
        result
            .commands
            .iter()
            .filter_map(|c| match c {
                DrawCommand::Text { position, .. } => Some(position.x.raw()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn ptab_three_region_header() {
        // The canonical Word header: left ⟶ ptab(center) ⟶ center ⟶
        // ptab(right) ⟶ right, all relative to the page margins.
        let frags = vec![
            text_frag("L", 10.0),
            ptab_frag(PTabAlignment::Center, PTabRelativeTo::Margin),
            text_frag("C", 20.0),
            ptab_frag(PTabAlignment::Right, PTabRelativeTo::Margin),
            text_frag("R", 30.0),
        ];
        let result = layout_paragraph(
            &frags,
            &body_constraints(100.0),
            &ParagraphStyle::default(),
            Pt::new(14.0),
            None,
        );

        let xs = text_xs(&result);
        assert_eq!(xs.len(), 3, "three text runs, no leaders");
        assert_eq!(xs[0], 0.0, "left run at the left margin");
        assert_eq!(xs[1], 40.0, "center run centered on 50 → 50 - 20/2");
        assert_eq!(xs[2], 70.0, "right run ends at the right margin → 100 - 30");
    }

    #[test]
    fn ptab_right_relative_to_indent() {
        // relativeTo=indent uses [indent_left, content_width - indent_right].
        let frags = vec![
            text_frag("x", 10.0),
            ptab_frag(PTabAlignment::Right, PTabRelativeTo::Indent),
            text_frag("yy", 20.0),
        ];
        let style = ParagraphStyle {
            indent_right: Pt::new(30.0),
            ..Default::default()
        };
        let result = layout_paragraph(
            &frags,
            &body_constraints(100.0),
            &style,
            Pt::new(14.0),
            None,
        );

        let xs = text_xs(&result);
        // span_end = 100 - 30 = 70; right run of width 20 ends there → 50.
        assert_eq!(xs[1], 50.0, "right run ends at the right indent");
    }

    #[test]
    fn ptab_suppresses_paragraph_alignment() {
        // A line with a ptab is positioned by the ptab, not by the paragraph's
        // alignment (§17.3.1.37 rationale) — the left run stays at the margin.
        let frags = vec![
            text_frag("L", 10.0),
            ptab_frag(PTabAlignment::Right, PTabRelativeTo::Margin),
            text_frag("R", 30.0),
        ];
        let style = ParagraphStyle {
            alignment: Alignment::End,
            ..Default::default()
        };
        let result = layout_paragraph(
            &frags,
            &body_constraints(100.0),
            &style,
            Pt::new(14.0),
            None,
        );

        let xs = text_xs(&result);
        assert_eq!(xs[0], 0.0, "left run not shifted by End alignment");
        assert_eq!(xs[1], 70.0, "right run ends at the right margin");
    }
}
