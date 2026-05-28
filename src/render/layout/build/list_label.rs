//! List label injection — prepend bullet/number labels to paragraph fragments.
//!
//! §17.9.22: when a paragraph carries a numbering reference, resolve the label
//! text (or picture bullet) and inject it as the first fragment, followed by a
//! tab that advances to the body text indent position.

use std::rc::Rc;

use crate::model::{self, ParagraphProperties};
use crate::render::dimension::Pt;
use crate::render::layout::fragment::Fragment;

use super::convert::{pic_bullet_size, remap_legacy_font_chars, resolve_paragraph_defaults};
use super::{BuildContext, BuildState};

/// Inject list label fragments into a paragraph if it has a numbering reference.
///
/// Updates `fragments` (prepends label + tab), `merged_props` (overrides indentation
/// from the numbering level), and `state.list_counters` (increments/resets counters).
pub(super) fn inject_list_label(
    para: &model::Paragraph,
    fragments: &mut Vec<Fragment>,
    merged_props: &mut ParagraphProperties,
    ctx: &BuildContext,
    state: &mut BuildState,
) {
    let num_ref = match merged_props.numbering {
        Some(ref nr) => nr,
        None => return,
    };

    let num_id = model::NumId::new(num_ref.num_id);
    let level = num_ref.level;

    let levels = match ctx.resolved.numbering.get(&num_id) {
        Some(levels) => levels,
        None => return,
    };

    // Update counters: increment this level, reset deeper levels.
    {
        let counters = &mut state.list_counters;
        let count = counters
            .entry((num_id, level))
            .or_insert_with(|| levels.get(level as usize).map(|l| l.start).unwrap_or(1) - 1);
        *count += 1;
        // Reset deeper levels.
        let max_level = levels.len() as u8;
        for deeper in (level + 1)..max_level {
            counters.remove(&(num_id, deeper));
        }
    }

    let level_def = levels.get(level as usize);

    // §17.9.10: check for picture bullet before text label.
    let pic_bullet_injected = level_def
        .and_then(|l| l.lvl_pic_bullet_id)
        .and_then(|pic_id| ctx.resolved.pic_bullets.get(&pic_id))
        .and_then(|bullet| {
            let rel_id = bullet
                .pict
                .as_ref()?
                .shapes()
                .next()?
                .common
                .image_data
                .as_ref()?
                .rel_id
                .as_ref()?;
            let image_bytes = ctx.media().get(rel_id)?;
            // Size from VML shape style (width/height), default 9pt.
            let size = pic_bullet_size(bullet);
            let label_frag = Fragment::Image {
                size,
                rel_id: rel_id.as_str().to_string(),
                image_data: Some(image_bytes.clone()),
            };
            Some((label_frag, size.height))
        });

    if let Some((label_frag, label_height)) = pic_bullet_injected {
        let hanging = extract_hanging(level_def);
        let tab_frag = Fragment::Tab {
            line_height: label_height,
            fitting_width: Some(hanging),
        };
        fragments.insert(0, tab_frag);
        fragments.insert(0, label_frag);

        if let Some(lvl_left) = level_def
            .and_then(|l| l.indentation.as_ref())
            .and_then(|ind| ind.start)
        {
            merged_props.tabs.insert(
                0,
                crate::model::TabStop {
                    position: lvl_left,
                    alignment: crate::model::TabAlignment::Left,
                    leader: crate::model::TabLeader::None,
                },
            );
        }
    } else {
        inject_text_label(
            para,
            fragments,
            merged_props,
            ctx,
            &state.list_counters,
            levels,
            level,
            level_def,
        );
    }

    // §17.9.23: numbering level pPr overrides the paragraph style.
    // Only the paragraph's direct ind overrides the numbering level.
    if let Some(lvl_ind) = levels
        .get(level as usize)
        .and_then(|l| l.indentation.as_ref())
    {
        let mut ind = *lvl_ind;
        if let Some(direct) = para.properties.indentation {
            if let Some(start) = direct.start {
                ind.start = Some(start);
            }
            if let Some(end) = direct.end {
                ind.end = Some(end);
            }
            if let Some(first_line) = direct.first_line {
                ind.first_line = Some(first_line);
            }
        }
        merged_props.indentation = Some(ind);
    }
}

/// Inject a text label (non-picture bullet) into the paragraph fragments.
#[allow(clippy::too_many_arguments)]
fn inject_text_label(
    para: &model::Paragraph,
    fragments: &mut Vec<Fragment>,
    merged_props: &mut ParagraphProperties,
    ctx: &BuildContext,
    counters: &std::collections::HashMap<(model::NumId, u8), u32>,
    levels: &[crate::render::resolve::numbering::ResolvedNumberingLevel],
    level: u8,
    level_def: Option<&crate::render::resolve::numbering::ResolvedNumberingLevel>,
) {
    let num_id = model::NumId::new(merged_props.numbering.as_ref().unwrap().num_id);
    let label_text =
        match crate::render::resolve::numbering::format_list_label(levels, level, counters, num_id)
        {
            Some(t) => t,
            None => return,
        };

    let (default_family, default_size, default_color, _, paragraph_style_run) =
        resolve_paragraph_defaults(para, ctx.resolved, false);

    // §17.9.23 / §17.3.1.29: assemble the label's character-property
    // cascade. Order matters — level rPr beats paragraph-mark rPr beats
    // paragraph-style run defaults. Document defaults arrive below via
    // `default_family`/`default_size` scalars consumed by
    // `font_props_from_run`.
    let cascade = ListLabelRunPropertyCascade {
        level: level_def.and_then(|l| l.run_properties.as_ref()),
        paragraph_mark: para.mark_run_properties.as_ref(),
        paragraph_style: Some(&paragraph_style_run),
    };

    // §17.3.2.6: color follows the same cascade as the font fields,
    // resolved as a separate scalar because it isn't part of
    // `FontProps`.
    let label_color = cascade
        .pick(|rp| rp.color)
        .map(|c| {
            crate::render::resolve::color::resolve_color(
                c,
                crate::render::resolve::color::ColorContext::Text,
            )
        })
        .unwrap_or(default_color);

    // Legacy Symbol/Wingdings remapping needs the cascade-resolved
    // family before deciding whether to remap PUA codepoints back to
    // their bullet glyphs.
    let cascade_family = cascade
        .iter()
        .find_map(|rp| crate::render::resolve::fonts::effective_font(&rp.fonts))
        .unwrap_or("");
    let (label_text, label_family) =
        remap_legacy_font_chars(&label_text, cascade_family, &default_family);

    // Build the label font through the canonical path so every
    // label-relevant field (underline, char_spacing, text_scale, etc.)
    // flows from cascade.resolve() into `font_props_from_run`. After
    // remapping, override the family if it changed.
    let mut label_font = build_label_font_props(&cascade, &default_family, default_size);
    if label_family != *label_font.family {
        label_font.family = Rc::from(label_family.as_str());
    }
    // §17.3.2.40: populate underline metrics from font metrics now that
    // the bool is settled — `populate_underline_metrics` (used by
    // `build_fragments`) ran before label injection, so this fragment
    // must populate its own metrics.
    populate_label_underline_metrics(&mut label_font, ctx.measurer);

    let (w, m) = ctx.measurer.measure(&label_text, &label_font);
    let h = m.height();

    let hanging = extract_hanging(level_def);
    // §17.9.7: lvlJc controls label justification within the hanging indent area.
    let jc = level_def.and_then(|l| l.justification);
    let text_offset = match jc {
        Some(crate::model::Alignment::End) => -w,
        Some(crate::model::Alignment::Center) => w * -0.5,
        _ => Pt::ZERO,
    };
    let label_width = w;
    let label_frag = Fragment::Text {
        text: Rc::from(label_text.as_str()),
        font: label_font.clone(),
        color: label_color,
        shading: None,
        border: None,
        width: label_width,
        trimmed_width: label_width,
        metrics: m,
        hyperlink_url: None,
        baseline_offset: Pt::ZERO,
        text_offset,
    };
    let tab_fitting = (hanging - label_width).max(Pt::ZERO);
    let tab_frag = Fragment::Tab {
        line_height: h,
        fitting_width: Some(tab_fitting),
    };
    fragments.insert(0, tab_frag);
    fragments.insert(0, label_frag);

    // Add implicit tab stop at numLvl.left so the tab lands at the body text position.
    let lvl_left = level_def
        .and_then(|l| l.indentation.as_ref())
        .and_then(|ind| ind.start);
    if let Some(lvl_left) = lvl_left {
        merged_props.tabs.insert(
            0,
            crate::model::TabStop {
                position: lvl_left,
                alignment: crate::model::TabAlignment::Left,
                leader: crate::model::TabLeader::None,
            },
        );
    }
}

/// Extract the hanging indent from a numbering level definition.
fn extract_hanging(
    level_def: Option<&crate::render::resolve::numbering::ResolvedNumberingLevel>,
) -> Pt {
    level_def
        .and_then(|l| l.indentation.as_ref())
        .and_then(|ind| ind.first_line)
        .map(|fl| match fl {
            model::FirstLineIndent::Hanging(v) => Pt::from(v),
            _ => Pt::ZERO,
        })
        .unwrap_or(Pt::ZERO)
}

/// §17.9.23 — derive a label's [`FontProps`] from the resolved
/// character-property cascade. Delegates to [`font_props_from_run`]
/// so every label-relevant field (bold, italic, underline,
/// char_spacing, text_scale, font family, font size) takes the same
/// code path as ordinary text-run formatting. The returned
/// [`FontProps`] still has `underline_position`/`underline_thickness`
/// at zero — those come from font metrics and require a measurer.
/// Use [`populate_label_underline_metrics`] to fill them.
pub(super) fn build_label_font_props(
    cascade: &ListLabelRunPropertyCascade<'_>,
    default_family: &str,
    default_size: Pt,
) -> crate::render::layout::fragment::FontProps {
    let effective = cascade.resolve();
    crate::render::layout::fragment::font_props_from_run(&effective, default_family, default_size)
}

/// Populate `underline_position` and `underline_thickness` on a
/// label's [`FontProps`] from the measurer's font metrics. No-op when
/// the label is not underlined. Idempotent.
pub(super) fn populate_label_underline_metrics(
    font: &mut crate::render::layout::fragment::FontProps,
    measurer: &crate::render::layout::measurer::TextMeasurer,
) {
    if font.underline {
        let (pos, thickness) = measurer.underline_metrics(font);
        font.underline_position = pos;
        font.underline_thickness = thickness;
    }
}

/// §17.9.23 / §17.3.1.29 — character-property cascade for a list
/// label's text. Sources are listed in priority order (highest first);
/// each field of the resolved properties is the topmost `Some` value
/// across `level → paragraph_mark → paragraph_style`.
///
/// The cascade is the spec-defined chain: a `<w:lvl><w:rPr>` overrides
/// the paragraph-mark `<w:pPr><w:rPr>`, which in turn overrides the
/// paragraph style's run defaults. Document defaults and theme
/// fallbacks come in below as scalar `default_family`/`default_size`
/// values supplied to `font_props_from_run`.
pub(super) struct ListLabelRunPropertyCascade<'a> {
    /// `<w:lvl><w:rPr>` — top of the cascade (§17.9.23).
    pub level: Option<&'a model::RunProperties>,
    /// `<w:pPr><w:rPr>` — formatting on the paragraph mark, applied
    /// where the level layer doesn't set a given field (§17.3.1.29).
    pub paragraph_mark: Option<&'a model::RunProperties>,
    /// Paragraph-style run defaults — bottom of the cascade.
    pub paragraph_style: Option<&'a model::RunProperties>,
}

impl<'a> ListLabelRunPropertyCascade<'a> {
    /// Iterate cascade sources in priority order, skipping `None`
    /// layers. Used by `pick` and by callers that need direct access
    /// to the cascade for non-`Copy` fields like `FontSet`.
    pub(super) fn iter(&self) -> impl Iterator<Item = &'a model::RunProperties> + '_ {
        [self.level, self.paragraph_mark, self.paragraph_style]
            .into_iter()
            .flatten()
    }

    /// Return the first `Some` value of a given field across the
    /// cascade, or `None` when no layer sets it. Restricted to `Copy`
    /// fields so the lookup is allocation-free; use `iter()` for fields
    /// that require cloning (e.g. `FontSet`).
    pub(super) fn pick<T: Copy>(
        &self,
        get: impl Fn(&model::RunProperties) -> Option<T>,
    ) -> Option<T> {
        self.iter().find_map(get)
    }

    /// Materialize a single effective `RunProperties` whose label-
    /// relevant fields (those consumed by `font_props_from_run` plus
    /// `color`) are picked from the highest-priority cascade source
    /// that sets each field. Fields no source sets are left at their
    /// default (`None` / empty `FontSet`) so downstream code can apply
    /// paragraph-level fallbacks.
    pub(super) fn resolve(&self) -> model::RunProperties {
        // `FontSet` is non-`Copy`. Pick from the first cascade source
        // whose font is explicitly set (any slot resolves under
        // `effective_font`); otherwise leave empty so downstream
        // `font_props_from_run` falls back to `default_family`.
        let fonts = self
            .iter()
            .find(|rp| crate::render::resolve::fonts::effective_font(&rp.fonts).is_some())
            .map(|rp| rp.fonts.clone())
            .unwrap_or_default();

        model::RunProperties {
            fonts,
            font_size: self.pick(|rp| rp.font_size),
            bold: self.pick(|rp| rp.bold),
            italic: self.pick(|rp| rp.italic),
            underline: self.pick(|rp| rp.underline),
            color: self.pick(|rp| rp.color),
            spacing: self.pick(|rp| rp.spacing),
            text_scale: self.pick(|rp| rp.text_scale),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::dimension::{Dimension, HalfPoints, Twips};
    use crate::model::{FontSet, FontSlot, RunProperties, TextScale, UnderlineStyle};

    fn rp_with_bold(b: bool) -> RunProperties {
        RunProperties {
            bold: Some(b),
            ..Default::default()
        }
    }

    fn rp_with_underline(u: UnderlineStyle) -> RunProperties {
        RunProperties {
            underline: Some(u),
            ..Default::default()
        }
    }

    fn rp_with_font(name: &str) -> RunProperties {
        RunProperties {
            fonts: FontSet {
                ascii: FontSlot::from_name(name),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Highest-priority source wins when more than one layer sets the
    /// same field.
    #[test]
    fn cascade_pick_level_overrides_mark_for_bold() {
        let level = rp_with_bold(true);
        let mark = rp_with_bold(false);
        let cascade = ListLabelRunPropertyCascade {
            level: Some(&level),
            paragraph_mark: Some(&mark),
            paragraph_style: None,
        };
        assert_eq!(cascade.pick(|rp| rp.bold), Some(true));
    }

    /// When the level layer doesn't set a field, the next layer down
    /// supplies it. §17.3.1.29.
    #[test]
    fn cascade_pick_falls_through_to_mark_when_level_field_absent() {
        let level = rp_with_bold(true); // sets bold but NOT underline
        let mark = rp_with_underline(UnderlineStyle::Single);
        let cascade = ListLabelRunPropertyCascade {
            level: Some(&level),
            paragraph_mark: Some(&mark),
            paragraph_style: None,
        };
        assert_eq!(cascade.pick(|rp| rp.bold), Some(true));
        assert_eq!(
            cascade.pick(|rp| rp.underline),
            Some(UnderlineStyle::Single)
        );
    }

    /// Paragraph-style run defaults are the bottom layer — used only
    /// when neither level nor mark sets a field.
    #[test]
    fn cascade_pick_falls_through_to_paragraph_style_when_others_absent() {
        let style = rp_with_underline(UnderlineStyle::Double);
        let cascade = ListLabelRunPropertyCascade {
            level: None,
            paragraph_mark: None,
            paragraph_style: Some(&style),
        };
        assert_eq!(
            cascade.pick(|rp| rp.underline),
            Some(UnderlineStyle::Double)
        );
    }

    /// No layer sets the field → `None`. Caller decides the default.
    #[test]
    fn cascade_pick_returns_none_when_no_source_sets_field() {
        let level = rp_with_bold(true);
        let cascade = ListLabelRunPropertyCascade {
            level: Some(&level),
            paragraph_mark: None,
            paragraph_style: None,
        };
        assert_eq!(cascade.pick(|rp| rp.italic), None);
    }

    /// `iter()` skips `None` layers and preserves priority order.
    #[test]
    fn cascade_iter_skips_none_layers_in_order() {
        let level = rp_with_bold(true);
        let style = rp_with_bold(false);
        let cascade = ListLabelRunPropertyCascade {
            level: Some(&level),
            paragraph_mark: None, // skipped
            paragraph_style: Some(&style),
        };
        let bolds: Vec<_> = cascade.iter().map(|rp| rp.bold).collect();
        assert_eq!(bolds, vec![Some(true), Some(false)]);
    }

    /// The key correctness property for the bug at hand: a level rPr
    /// underline must surface in the resolved properties so
    /// `font_props_from_run` reads it.
    #[test]
    fn cascade_resolve_materializes_underline_from_level() {
        let level = rp_with_underline(UnderlineStyle::Single);
        let cascade = ListLabelRunPropertyCascade {
            level: Some(&level),
            paragraph_mark: None,
            paragraph_style: None,
        };
        let effective = cascade.resolve();
        assert_eq!(effective.underline, Some(UnderlineStyle::Single));
    }

    /// `FontSet` (non-`Copy`) cascade rule: the resolved `fonts` is
    /// the first cascade source that has an explicitly set font slot.
    #[test]
    fn cascade_resolve_fonts_picks_first_explicit_source() {
        let level = RunProperties::default(); // no fonts
        let mark = rp_with_font("Verdana");
        let style = rp_with_font("Arial");
        let cascade = ListLabelRunPropertyCascade {
            level: Some(&level),
            paragraph_mark: Some(&mark),
            paragraph_style: Some(&style),
        };
        let effective = cascade.resolve();
        assert_eq!(effective.fonts.ascii.explicit.as_deref(), Some("Verdana"));
    }

    // ── §17.9.23 — `build_label_font_props` ──────────────────────────────
    //
    // The label's `FontProps` is built by passing the cascade-resolved
    // `RunProperties` through `font_props_from_run`. These tests pin
    // the spec-driven invariants the previous hand-rolled construction
    // violated (notably: dropped underline, char_spacing, text_scale).

    /// THE BUG: an underline in the level rPr (`<w:lvl><w:rPr><w:u/>`)
    /// must surface as `font.underline = true`. Previously this code
    /// path hardcoded `underline: false`.
    #[test]
    fn label_font_inherits_underline_from_level() {
        let level = rp_with_underline(UnderlineStyle::Single);
        let cascade = ListLabelRunPropertyCascade {
            level: Some(&level),
            paragraph_mark: None,
            paragraph_style: None,
        };
        let font = build_label_font_props(&cascade, "Helvetica", Pt::new(12.0));
        assert!(font.underline, "level rPr <w:u/> must become font.underline");
    }

    /// Cascade depth: when the level layer doesn't set underline but
    /// the paragraph-mark rPr does, the label inherits the underline.
    /// §17.3.1.29. (Previously broken: hand-rolled code never read
    /// either layer's underline.)
    #[test]
    fn label_font_inherits_underline_from_mark_when_level_absent() {
        let mark = rp_with_underline(UnderlineStyle::Single);
        let cascade = ListLabelRunPropertyCascade {
            level: None,
            paragraph_mark: Some(&mark),
            paragraph_style: None,
        };
        let font = build_label_font_props(&cascade, "Helvetica", Pt::new(12.0));
        assert!(font.underline);
    }

    /// Explicit `<w:u w:val="none"/>` in the level must turn underline
    /// OFF even if a lower cascade layer has it on — per spec, "none"
    /// is an explicit override, not an "inherit" signal. The cascade
    /// picks the level's `Some(None)` over the mark's `Some(Single)`,
    /// and `font_props_from_run` collapses it to `underline = false`.
    #[test]
    fn label_font_underline_false_when_level_explicitly_none() {
        let level = rp_with_underline(UnderlineStyle::None);
        let mark = rp_with_underline(UnderlineStyle::Single);
        let cascade = ListLabelRunPropertyCascade {
            level: Some(&level),
            paragraph_mark: Some(&mark),
            paragraph_style: None,
        };
        let font = build_label_font_props(&cascade, "Helvetica", Pt::new(12.0));
        assert!(
            !font.underline,
            "explicit UnderlineStyle::None must override lower layers"
        );
    }

    /// `<w:spacing>` (char spacing) was silently dropped by the
    /// hand-rolled construction. With cascade routing it must flow
    /// into `FontProps::char_spacing`.
    #[test]
    fn label_font_inherits_char_spacing_from_level() {
        let level = RunProperties {
            spacing: Some(Dimension::<Twips>::new(40)),
            ..Default::default()
        };
        let cascade = ListLabelRunPropertyCascade {
            level: Some(&level),
            paragraph_mark: None,
            paragraph_style: None,
        };
        let font = build_label_font_props(&cascade, "Helvetica", Pt::new(12.0));
        // 40 twips = 2 pt.
        assert!((font.char_spacing.raw() - 2.0).abs() < 1e-4);
    }

    /// `<w:w>` (text scale) was silently fixed to 1.0 in the
    /// hand-rolled construction.
    #[test]
    fn label_font_inherits_text_scale_from_level() {
        let level = RunProperties {
            text_scale: Some(TextScale::new(150)),
            ..Default::default()
        };
        let cascade = ListLabelRunPropertyCascade {
            level: Some(&level),
            paragraph_mark: None,
            paragraph_style: None,
        };
        let font = build_label_font_props(&cascade, "Helvetica", Pt::new(12.0));
        assert!((font.text_scale - 1.5).abs() < 1e-4);
    }

    /// Bold/italic/size pass through, with the cascade-resolved font
    /// family taking precedence over `default_family`.
    #[test]
    fn label_font_pass_through_basic_fields() {
        let level = RunProperties {
            bold: Some(true),
            italic: Some(true),
            font_size: Some(Dimension::<HalfPoints>::new(24)), // 12 pt
            fonts: FontSet {
                ascii: FontSlot::from_name("Verdana"),
                ..Default::default()
            },
            ..Default::default()
        };
        let cascade = ListLabelRunPropertyCascade {
            level: Some(&level),
            paragraph_mark: None,
            paragraph_style: None,
        };
        let font = build_label_font_props(&cascade, "Helvetica", Pt::new(10.0));
        assert!(font.bold);
        assert!(font.italic);
        assert_eq!(font.size.raw(), 12.0);
        assert_eq!(&*font.family, "Verdana");
    }

    /// When no cascade layer sets a font, `default_family` wins.
    #[test]
    fn label_font_falls_back_to_default_family() {
        let cascade = ListLabelRunPropertyCascade {
            level: None,
            paragraph_mark: None,
            paragraph_style: None,
        };
        let font = build_label_font_props(&cascade, "Helvetica", Pt::new(12.0));
        assert_eq!(&*font.family, "Helvetica");
    }

    /// Empty cascade — resolve returns default (all `None` / empty).
    #[test]
    fn cascade_resolve_returns_defaults_when_cascade_empty() {
        let cascade = ListLabelRunPropertyCascade {
            level: None,
            paragraph_mark: None,
            paragraph_style: None,
        };
        let effective = cascade.resolve();
        assert_eq!(effective, RunProperties::default());
    }

    /// All label-relevant fields composed across all three layers.
    /// Documents the full set of fields the resolver pulls (any
    /// future field becoming label-relevant should fail this test
    /// until added).
    #[test]
    fn cascade_resolve_composes_all_label_fields() {
        use crate::model::Color;

        let level = RunProperties {
            bold: Some(true),
            underline: Some(UnderlineStyle::Single),
            ..Default::default()
        };
        let mark = RunProperties {
            italic: Some(true),
            font_size: Some(Dimension::<HalfPoints>::new(24)),
            spacing: Some(Dimension::<Twips>::new(40)),
            ..Default::default()
        };
        let style = RunProperties {
            color: Some(Color::Rgb(0x112233)),
            text_scale: Some(TextScale::new(120)),
            fonts: FontSet {
                ascii: FontSlot::from_name("Calibri"),
                ..Default::default()
            },
            ..Default::default()
        };
        let cascade = ListLabelRunPropertyCascade {
            level: Some(&level),
            paragraph_mark: Some(&mark),
            paragraph_style: Some(&style),
        };
        let effective = cascade.resolve();
        assert_eq!(effective.bold, Some(true), "from level");
        assert_eq!(
            effective.underline,
            Some(UnderlineStyle::Single),
            "from level"
        );
        assert_eq!(effective.italic, Some(true), "from mark");
        assert_eq!(
            effective.font_size,
            Some(Dimension::<HalfPoints>::new(24)),
            "from mark"
        );
        assert_eq!(
            effective.spacing,
            Some(Dimension::<Twips>::new(40)),
            "from mark"
        );
        assert_eq!(
            effective.color,
            Some(Color::Rgb(0x112233)),
            "from style"
        );
        assert_eq!(effective.text_scale, Some(TextScale::new(120)), "from style");
        assert_eq!(
            effective.fonts.ascii.explicit.as_deref(),
            Some("Calibri"),
            "from style (lowest source that sets fonts)"
        );
    }
}
