//! Parser for DrawingML effects (§20.1.8.24 CT_EffectList).
//!
//! Reads the flat `effectLst` variant with its 8 child effect types. The
//! alternative `effectDag` (§20.1.8.25) is not parsed here — callers should
//! dispatch it separately (typically by logging and skipping).

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use crate::docx::dimension::Dimension;
use crate::docx::error::{ParseError, Result};
use crate::docx::model::{
    BlendMode, BlurEffect, DrawingColor, DrawingFill, Effect, EffectList, FillOverlayEffect,
    GlowEffect, InnerShadowEffect, OuterShadowEffect, PresetShadowEffect, PresetShadowVal,
    RectAlignment, ReflectionEffect, SoftEdgeEffect,
};
use crate::docx::xml;

use super::color::parse_color_choice;
use super::fill::parse_drawing_fill;

/// Parse `<a:effectLst>…</a:effectLst>`. Reader is positioned after the
/// Start event. Preserves document order.
pub fn parse_effect_list(reader: &mut Reader<&[u8]>, buf: &mut Vec<u8>) -> Result<EffectList> {
    let mut effects = Vec::new();
    loop {
        match xml::next_event(reader, buf)? {
            Event::Start(ref e) => {
                let local_owned = xml::local_name_owned(e.name().as_ref());
                if let Some(eff) = parse_effect_with_children(reader, buf, e, &local_owned)? {
                    effects.push(eff);
                } else {
                    xml::warn_unsupported_element("effectLst", &local_owned);
                    xml::skip_to_end(reader, buf, &local_owned)?;
                }
            }
            Event::Empty(ref e) => {
                let qn = e.name();
                let local = xml::local_name(qn.as_ref());
                if let Some(eff) = parse_effect_empty(e, local)? {
                    effects.push(eff);
                } else {
                    xml::warn_unsupported_element("effectLst", local);
                }
            }
            Event::End(ref e) if xml::local_name(e.name().as_ref()) == b"effectLst" => break,
            Event::Eof => return Err(xml::unexpected_eof(b"effectLst")),
            _ => {}
        }
    }
    Ok(EffectList { effects })
}

fn parse_effect_empty(e: &BytesStart<'_>, local: &[u8]) -> Result<Option<Effect>> {
    match local {
        b"blur" => Ok(Some(Effect::Blur(parse_blur_attrs(e)?))),
        b"reflection" => Ok(Some(Effect::Reflection(parse_reflection_attrs(e)?))),
        b"softEdge" => Ok(Some(Effect::SoftEdge(parse_soft_edge_attrs(e)?))),
        _ => Ok(None),
    }
}

fn parse_effect_with_children(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    start: &BytesStart<'_>,
    local: &[u8],
) -> Result<Option<Effect>> {
    match local {
        b"blur" => {
            let eff = parse_blur_attrs(start)?;
            xml::skip_to_end(reader, buf, b"blur")?;
            Ok(Some(Effect::Blur(eff)))
        }
        b"reflection" => {
            let eff = parse_reflection_attrs(start)?;
            xml::skip_to_end(reader, buf, b"reflection")?;
            Ok(Some(Effect::Reflection(eff)))
        }
        b"softEdge" => {
            let eff = parse_soft_edge_attrs(start)?;
            xml::skip_to_end(reader, buf, b"softEdge")?;
            Ok(Some(Effect::SoftEdge(eff)))
        }
        b"fillOverlay" => Ok(Some(Effect::FillOverlay(parse_fill_overlay(
            reader, buf, start,
        )?))),
        b"glow" => Ok(Some(Effect::Glow(parse_glow(reader, buf, start)?))),
        b"innerShdw" => Ok(Some(Effect::InnerShdw(parse_inner_shadow(
            reader, buf, start,
        )?))),
        b"outerShdw" => Ok(Some(Effect::OuterShdw(parse_outer_shadow(
            reader, buf, start,
        )?))),
        b"prstShdw" => Ok(Some(Effect::PrstShdw(parse_preset_shadow(
            reader, buf, start,
        )?))),
        _ => Ok(None),
    }
}

// ── Effect parsers ──────────────────────────────────────────────────────────

fn parse_blur_attrs(e: &BytesStart<'_>) -> Result<BlurEffect> {
    Ok(BlurEffect {
        radius: Dimension::new(xml::optional_attr_i64(e, b"rad")?.unwrap_or(0)),
        grow: xml::optional_attr_bool(e, b"grow")?,
    })
}

fn parse_soft_edge_attrs(e: &BytesStart<'_>) -> Result<SoftEdgeEffect> {
    Ok(SoftEdgeEffect {
        radius: Dimension::new(xml::optional_attr_i64(e, b"rad")?.unwrap_or(0)),
    })
}

fn parse_reflection_attrs(e: &BytesStart<'_>) -> Result<ReflectionEffect> {
    Ok(ReflectionEffect {
        blur_radius: Dimension::new(xml::optional_attr_i64(e, b"blurRad")?.unwrap_or(0)),
        start_alpha: Dimension::new(xml::optional_attr_i64(e, b"stA")?.unwrap_or(100_000)),
        start_pos: Dimension::new(xml::optional_attr_i64(e, b"stPos")?.unwrap_or(0)),
        end_alpha: Dimension::new(xml::optional_attr_i64(e, b"endA")?.unwrap_or(0)),
        end_pos: Dimension::new(xml::optional_attr_i64(e, b"endPos")?.unwrap_or(100_000)),
        distance: Dimension::new(xml::optional_attr_i64(e, b"dist")?.unwrap_or(0)),
        direction: Dimension::new(xml::optional_attr_i64(e, b"dir")?.unwrap_or(0)),
        fade_direction: Dimension::new(xml::optional_attr_i64(e, b"fadeDir")?.unwrap_or(5_400_000)),
        sx: Dimension::new(xml::optional_attr_i64(e, b"sx")?.unwrap_or(100_000)),
        sy: Dimension::new(xml::optional_attr_i64(e, b"sy")?.unwrap_or(100_000)),
        kx: Dimension::new(xml::optional_attr_i64(e, b"kx")?.unwrap_or(0)),
        ky: Dimension::new(xml::optional_attr_i64(e, b"ky")?.unwrap_or(0)),
        alignment: xml::optional_attr(e, b"algn")?
            .map(|s| parse_rect_alignment(&s))
            .transpose()?
            .unwrap_or(RectAlignment::B),
        rot_with_shape: xml::optional_attr_bool(e, b"rotWithShape")?,
    })
}

fn parse_fill_overlay(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    start: &BytesStart<'_>,
) -> Result<FillOverlayEffect> {
    let blend = xml::optional_attr(start, b"blend")?
        .map(|s| parse_blend_mode(&s))
        .transpose()?
        .ok_or_else(|| ParseError::MissingAttribute {
            element: "a:fillOverlay".into(),
            attr: "blend".into(),
        })?;
    let fill = parse_single_fill_child(reader, buf, b"fillOverlay")?;
    Ok(FillOverlayEffect { fill, blend })
}

fn parse_glow(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    start: &BytesStart<'_>,
) -> Result<GlowEffect> {
    let radius = Dimension::new(xml::optional_attr_i64(start, b"rad")?.unwrap_or(0));
    let color = parse_single_color_child(reader, buf, b"glow")?;
    Ok(GlowEffect { radius, color })
}

fn parse_inner_shadow(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    start: &BytesStart<'_>,
) -> Result<InnerShadowEffect> {
    let blur_radius = Dimension::new(xml::optional_attr_i64(start, b"blurRad")?.unwrap_or(0));
    let distance = Dimension::new(xml::optional_attr_i64(start, b"dist")?.unwrap_or(0));
    let direction = Dimension::new(xml::optional_attr_i64(start, b"dir")?.unwrap_or(0));
    let color = parse_single_color_child(reader, buf, b"innerShdw")?;
    Ok(InnerShadowEffect {
        blur_radius,
        distance,
        direction,
        color,
    })
}

fn parse_outer_shadow(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    start: &BytesStart<'_>,
) -> Result<OuterShadowEffect> {
    let blur_radius = Dimension::new(xml::optional_attr_i64(start, b"blurRad")?.unwrap_or(0));
    let distance = Dimension::new(xml::optional_attr_i64(start, b"dist")?.unwrap_or(0));
    let direction = Dimension::new(xml::optional_attr_i64(start, b"dir")?.unwrap_or(0));
    let sx = Dimension::new(xml::optional_attr_i64(start, b"sx")?.unwrap_or(100_000));
    let sy = Dimension::new(xml::optional_attr_i64(start, b"sy")?.unwrap_or(100_000));
    let kx = Dimension::new(xml::optional_attr_i64(start, b"kx")?.unwrap_or(0));
    let ky = Dimension::new(xml::optional_attr_i64(start, b"ky")?.unwrap_or(0));
    let alignment = xml::optional_attr(start, b"algn")?
        .map(|s| parse_rect_alignment(&s))
        .transpose()?
        .unwrap_or(RectAlignment::B);
    let rot_with_shape = xml::optional_attr_bool(start, b"rotWithShape")?;
    let color = parse_single_color_child(reader, buf, b"outerShdw")?;
    Ok(OuterShadowEffect {
        blur_radius,
        distance,
        direction,
        sx,
        sy,
        kx,
        ky,
        alignment,
        rot_with_shape,
        color,
    })
}

fn parse_preset_shadow(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    start: &BytesStart<'_>,
) -> Result<PresetShadowEffect> {
    let preset =
        parse_preset_shadow_val(&xml::optional_attr(start, b"prst")?.ok_or_else(|| {
            ParseError::MissingAttribute {
                element: "a:prstShdw".into(),
                attr: "prst".into(),
            }
        })?)?;
    let distance = Dimension::new(xml::optional_attr_i64(start, b"dist")?.unwrap_or(0));
    let direction = Dimension::new(xml::optional_attr_i64(start, b"dir")?.unwrap_or(0));
    let color = parse_single_color_child(reader, buf, b"prstShdw")?;
    Ok(PresetShadowEffect {
        preset,
        distance,
        direction,
        color,
    })
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn parse_single_color_child(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    end_tag: &[u8],
) -> Result<DrawingColor> {
    let mut color: Option<DrawingColor> = None;
    loop {
        match xml::next_event(reader, buf)? {
            Event::Start(ref e) => {
                let local_owned = xml::local_name_owned(e.name().as_ref());
                if let Some(c) = parse_color_choice(reader, buf, e, false)? {
                    color = Some(c);
                } else {
                    xml::warn_unsupported_element("effect", &local_owned);
                    xml::skip_to_end(reader, buf, &local_owned)?;
                }
            }
            Event::Empty(ref e) => {
                let local_owned = xml::local_name_owned(e.name().as_ref());
                if let Some(c) = parse_color_choice(reader, buf, e, true)? {
                    color = Some(c);
                } else {
                    xml::warn_unsupported_element("effect", &local_owned);
                }
            }
            Event::End(ref e) if xml::local_name(e.name().as_ref()) == end_tag => break,
            Event::Eof => return Err(xml::unexpected_eof(end_tag)),
            _ => {}
        }
    }
    color.ok_or_else(|| ParseError::MissingElement {
        parent: String::from_utf8_lossy(end_tag).into_owned(),
        child: "color".into(),
    })
}

fn parse_single_fill_child(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    end_tag: &[u8],
) -> Result<DrawingFill> {
    let mut fill: Option<DrawingFill> = None;
    loop {
        match xml::next_event(reader, buf)? {
            Event::Start(ref e) => {
                let local_owned = xml::local_name_owned(e.name().as_ref());
                if let Some(f) = parse_drawing_fill(reader, buf, e, false)? {
                    fill = Some(f);
                } else {
                    xml::warn_unsupported_element("fillOverlay", &local_owned);
                    xml::skip_to_end(reader, buf, &local_owned)?;
                }
            }
            Event::Empty(ref e) => {
                let local_owned = xml::local_name_owned(e.name().as_ref());
                if let Some(f) = parse_drawing_fill(reader, buf, e, true)? {
                    fill = Some(f);
                } else {
                    xml::warn_unsupported_element("fillOverlay", &local_owned);
                }
            }
            Event::End(ref e) if xml::local_name(e.name().as_ref()) == end_tag => break,
            Event::Eof => return Err(xml::unexpected_eof(end_tag)),
            _ => {}
        }
    }
    fill.ok_or_else(|| ParseError::MissingElement {
        parent: String::from_utf8_lossy(end_tag).into_owned(),
        child: "fill".into(),
    })
}

fn parse_rect_alignment(val: &str) -> Result<RectAlignment> {
    match val {
        "tl" => Ok(RectAlignment::Tl),
        "t" => Ok(RectAlignment::T),
        "tr" => Ok(RectAlignment::Tr),
        "l" => Ok(RectAlignment::L),
        "ctr" => Ok(RectAlignment::Ctr),
        "r" => Ok(RectAlignment::R),
        "bl" => Ok(RectAlignment::Bl),
        "b" => Ok(RectAlignment::B),
        "br" => Ok(RectAlignment::Br),
        other => Err(ParseError::InvalidAttributeValue {
            attr: "algn".into(),
            value: other.into(),
            reason: "expected value per §20.1.10.53 ST_RectAlignment".into(),
        }),
    }
}

fn parse_blend_mode(val: &str) -> Result<BlendMode> {
    match val {
        "over" => Ok(BlendMode::Over),
        "mult" => Ok(BlendMode::Mult),
        "screen" => Ok(BlendMode::Screen),
        "darken" => Ok(BlendMode::Darken),
        "lighten" => Ok(BlendMode::Lighten),
        other => Err(ParseError::InvalidAttributeValue {
            attr: "blend".into(),
            value: other.into(),
            reason: "expected value per §20.1.10.11 ST_BlendMode".into(),
        }),
    }
}

fn parse_preset_shadow_val(val: &str) -> Result<PresetShadowVal> {
    match val {
        "shdw1" => Ok(PresetShadowVal::Shdw1),
        "shdw2" => Ok(PresetShadowVal::Shdw2),
        "shdw3" => Ok(PresetShadowVal::Shdw3),
        "shdw4" => Ok(PresetShadowVal::Shdw4),
        "shdw5" => Ok(PresetShadowVal::Shdw5),
        "shdw6" => Ok(PresetShadowVal::Shdw6),
        "shdw7" => Ok(PresetShadowVal::Shdw7),
        "shdw8" => Ok(PresetShadowVal::Shdw8),
        "shdw9" => Ok(PresetShadowVal::Shdw9),
        "shdw10" => Ok(PresetShadowVal::Shdw10),
        "shdw11" => Ok(PresetShadowVal::Shdw11),
        "shdw12" => Ok(PresetShadowVal::Shdw12),
        "shdw13" => Ok(PresetShadowVal::Shdw13),
        "shdw14" => Ok(PresetShadowVal::Shdw14),
        "shdw15" => Ok(PresetShadowVal::Shdw15),
        "shdw16" => Ok(PresetShadowVal::Shdw16),
        "shdw17" => Ok(PresetShadowVal::Shdw17),
        "shdw18" => Ok(PresetShadowVal::Shdw18),
        "shdw19" => Ok(PresetShadowVal::Shdw19),
        "shdw20" => Ok(PresetShadowVal::Shdw20),
        other => Err(ParseError::InvalidAttributeValue {
            attr: "prst".into(),
            value: other.into(),
            reason: "expected value per §20.1.10.51 ST_PresetShadowVal".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(xml_src: &str) -> EffectList {
        let mut reader = Reader::from_reader(xml_src.as_bytes());
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        loop {
            match xml::next_event(&mut reader, &mut buf).unwrap() {
                Event::Start(ref e) if xml::local_name(e.name().as_ref()) == b"effectLst" => {
                    return parse_effect_list(&mut reader, &mut buf).unwrap();
                }
                Event::Eof => panic!("no effectLst element"),
                _ => {}
            }
        }
    }

    #[test]
    fn empty_effect_list() {
        let l = parse(r#"<a:effectLst xmlns:a="urn:a"></a:effectLst>"#);
        assert!(l.effects.is_empty());
    }

    #[test]
    fn blur_effect_empty() {
        let l = parse(
            r#"<a:effectLst xmlns:a="urn:a">
                <a:blur rad="50800" grow="1"/>
            </a:effectLst>"#,
        );
        assert_eq!(l.effects.len(), 1);
        let Effect::Blur(b) = &l.effects[0] else {
            panic!()
        };
        assert_eq!(b.radius.raw(), 50_800);
        assert_eq!(b.grow, Some(true));
    }

    #[test]
    fn outer_shadow_with_color() {
        let l = parse(
            r#"<a:effectLst xmlns:a="urn:a">
                <a:outerShdw blurRad="50800" dist="38100" dir="5400000" algn="tl" rotWithShape="0">
                    <a:srgbClr val="000000"/>
                </a:outerShdw>
            </a:effectLst>"#,
        );
        let Effect::OuterShdw(s) = &l.effects[0] else {
            panic!()
        };
        assert_eq!(s.blur_radius.raw(), 50_800);
        assert_eq!(s.distance.raw(), 38_100);
        assert_eq!(s.direction.raw(), 5_400_000);
        assert_eq!(s.alignment, RectAlignment::Tl);
        assert_eq!(s.rot_with_shape, Some(false));
    }

    #[test]
    fn inner_shadow_with_color() {
        let l = parse(
            r#"<a:effectLst xmlns:a="urn:a">
                <a:innerShdw blurRad="63500" dist="50800" dir="2700000">
                    <a:srgbClr val="333333"/>
                </a:innerShdw>
            </a:effectLst>"#,
        );
        let Effect::InnerShdw(s) = &l.effects[0] else {
            panic!()
        };
        assert_eq!(s.blur_radius.raw(), 63_500);
    }

    #[test]
    fn glow_effect() {
        let l = parse(
            r#"<a:effectLst xmlns:a="urn:a">
                <a:glow rad="40000">
                    <a:srgbClr val="FFC000"/>
                </a:glow>
            </a:effectLst>"#,
        );
        let Effect::Glow(g) = &l.effects[0] else {
            panic!()
        };
        assert_eq!(g.radius.raw(), 40_000);
        assert!(matches!(g.color, DrawingColor::Srgb { rgb: 0xFFC000, .. }));
    }

    #[test]
    fn preset_shadow() {
        let l = parse(
            r#"<a:effectLst xmlns:a="urn:a">
                <a:prstShdw prst="shdw5" dist="38100" dir="5400000">
                    <a:srgbClr val="000000"/>
                </a:prstShdw>
            </a:effectLst>"#,
        );
        let Effect::PrstShdw(s) = &l.effects[0] else {
            panic!()
        };
        assert_eq!(s.preset, PresetShadowVal::Shdw5);
    }

    #[test]
    fn reflection_empty_uses_defaults() {
        let l = parse(r#"<a:effectLst xmlns:a="urn:a"><a:reflection/></a:effectLst>"#);
        let Effect::Reflection(r) = &l.effects[0] else {
            panic!()
        };
        assert_eq!(r.start_alpha.raw(), 100_000); // default stA
        assert_eq!(r.end_pos.raw(), 100_000); // default endPos
        assert_eq!(r.alignment, RectAlignment::B);
    }

    #[test]
    fn soft_edge() {
        let l = parse(r#"<a:effectLst xmlns:a="urn:a"><a:softEdge rad="25400"/></a:effectLst>"#);
        let Effect::SoftEdge(s) = &l.effects[0] else {
            panic!()
        };
        assert_eq!(s.radius.raw(), 25_400);
    }

    #[test]
    fn fill_overlay_multiply() {
        let l = parse(
            r#"<a:effectLst xmlns:a="urn:a">
                <a:fillOverlay blend="mult">
                    <a:solidFill><a:srgbClr val="FF0000"/></a:solidFill>
                </a:fillOverlay>
            </a:effectLst>"#,
        );
        let Effect::FillOverlay(fo) = &l.effects[0] else {
            panic!()
        };
        assert_eq!(fo.blend, BlendMode::Mult);
        assert!(matches!(fo.fill, DrawingFill::Solid(_)));
    }

    #[test]
    fn multi_effect_order_preserved() {
        let l = parse(
            r#"<a:effectLst xmlns:a="urn:a">
                <a:outerShdw><a:srgbClr val="000000"/></a:outerShdw>
                <a:glow rad="1000"><a:srgbClr val="FF0000"/></a:glow>
                <a:softEdge rad="500"/>
            </a:effectLst>"#,
        );
        assert_eq!(l.effects.len(), 3);
        assert!(matches!(l.effects[0], Effect::OuterShdw(_)));
        assert!(matches!(l.effects[1], Effect::Glow(_)));
        assert!(matches!(l.effects[2], Effect::SoftEdge(_)));
    }

    #[test]
    fn invalid_rect_alignment_errors() {
        let mut reader = Reader::from_reader(
            r#"<a:effectLst xmlns:a="urn:a">
                <a:outerShdw algn="bogus"><a:srgbClr val="000000"/></a:outerShdw>
            </a:effectLst>"#
                .as_bytes(),
        );
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        loop {
            match xml::next_event(&mut reader, &mut buf).unwrap() {
                Event::Start(ref e) if xml::local_name(e.name().as_ref()) == b"effectLst" => {
                    assert!(parse_effect_list(&mut reader, &mut buf).is_err());
                    return;
                }
                Event::Eof => panic!(),
                _ => {}
            }
        }
    }

    #[test]
    fn invalid_blend_mode_errors() {
        let mut reader = Reader::from_reader(
            r#"<a:effectLst xmlns:a="urn:a">
                <a:fillOverlay blend="bogus"><a:noFill/></a:fillOverlay>
            </a:effectLst>"#
                .as_bytes(),
        );
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        loop {
            match xml::next_event(&mut reader, &mut buf).unwrap() {
                Event::Start(ref e) if xml::local_name(e.name().as_ref()) == b"effectLst" => {
                    assert!(parse_effect_list(&mut reader, &mut buf).is_err());
                    return;
                }
                Event::Eof => panic!(),
                _ => {}
            }
        }
    }
}
