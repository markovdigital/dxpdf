//! Parser for DrawingML outlines (§20.1.2.2.24 CT_LineProperties).
//!
//! Full outline with width/cap/compound/align attributes, fill (via the
//! shared `parse_drawing_fill`), dash (preset or custom), join (round/bevel/
//! miter), and head/tail end arrows.

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use crate::docx::dimension::Dimension;
use crate::docx::error::{ParseError, Result};
use crate::docx::model::{
    CompoundLine, DashStop, DrawingFill, LineCap, LineDash, LineEnd, LineEndSize, LineEndType,
    LineJoin, Outline, PenAlignment, PresetLineDashVal,
};
use crate::docx::xml;

use super::fill::parse_drawing_fill;

/// Parse `<a:ln>…</a:ln>` (CT_LineProperties). Reader is positioned after
/// the Start event; `start` supplies the attribute bag.
pub fn parse_outline(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    start: &BytesStart<'_>,
) -> Result<Outline> {
    let width = xml::optional_attr_i64(start, b"w")?.map(Dimension::new);
    let cap = xml::optional_attr(start, b"cap")?
        .map(|s| parse_line_cap(&s))
        .transpose()?;
    let compound = xml::optional_attr(start, b"cmpd")?
        .map(|s| parse_compound_line(&s))
        .transpose()?;
    let alignment = xml::optional_attr(start, b"algn")?
        .map(|s| parse_pen_alignment(&s))
        .transpose()?;

    let mut fill: Option<DrawingFill> = None;
    let mut dash: Option<LineDash> = None;
    let mut join: Option<LineJoin> = None;
    let mut head_end: Option<LineEnd> = None;
    let mut tail_end: Option<LineEnd> = None;

    loop {
        match xml::next_event(reader, buf)? {
            Event::Start(ref e) => {
                let local_owned = xml::local_name_owned(e.name().as_ref());
                match &*local_owned {
                    b"noFill" | b"solidFill" | b"gradFill" | b"blipFill" | b"pattFill"
                    | b"grpFill" => {
                        fill = parse_drawing_fill(reader, buf, e, false)?;
                    }
                    b"prstDash" => {
                        dash = Some(LineDash::Preset(parse_preset_line_dash_val(
                            &xml::optional_attr(e, b"val")?.ok_or_else(|| {
                                ParseError::MissingAttribute {
                                    element: "a:prstDash".into(),
                                    attr: "val".into(),
                                }
                            })?,
                        )?));
                        xml::skip_to_end(reader, buf, b"prstDash")?;
                    }
                    b"custDash" => {
                        dash = Some(LineDash::Custom(parse_custom_dash_stops(reader, buf)?));
                    }
                    b"round" => {
                        join = Some(LineJoin::Round);
                        xml::skip_to_end(reader, buf, b"round")?;
                    }
                    b"bevel" => {
                        join = Some(LineJoin::Bevel);
                        xml::skip_to_end(reader, buf, b"bevel")?;
                    }
                    b"miter" => {
                        join = Some(LineJoin::Miter {
                            limit: xml::optional_attr_i64(e, b"lim")?.map(Dimension::new),
                        });
                        xml::skip_to_end(reader, buf, b"miter")?;
                    }
                    b"headEnd" => {
                        head_end = Some(parse_line_end(e)?);
                        xml::skip_to_end(reader, buf, b"headEnd")?;
                    }
                    b"tailEnd" => {
                        tail_end = Some(parse_line_end(e)?);
                        xml::skip_to_end(reader, buf, b"tailEnd")?;
                    }
                    _ => {
                        xml::warn_unsupported_element("ln", &local_owned);
                        xml::skip_to_end(reader, buf, &local_owned)?;
                    }
                }
            }
            Event::Empty(ref e) => {
                let qn = e.name();
                let local = xml::local_name(qn.as_ref());
                match local {
                    b"noFill" => fill = Some(DrawingFill::None),
                    b"grpFill" => fill = Some(DrawingFill::Group),
                    b"prstDash" => {
                        dash = Some(LineDash::Preset(parse_preset_line_dash_val(
                            &xml::optional_attr(e, b"val")?.ok_or_else(|| {
                                ParseError::MissingAttribute {
                                    element: "a:prstDash".into(),
                                    attr: "val".into(),
                                }
                            })?,
                        )?));
                    }
                    b"round" => join = Some(LineJoin::Round),
                    b"bevel" => join = Some(LineJoin::Bevel),
                    b"miter" => {
                        join = Some(LineJoin::Miter {
                            limit: xml::optional_attr_i64(e, b"lim")?.map(Dimension::new),
                        });
                    }
                    b"headEnd" => head_end = Some(parse_line_end(e)?),
                    b"tailEnd" => tail_end = Some(parse_line_end(e)?),
                    _ => xml::warn_unsupported_element("ln", local),
                }
            }
            Event::End(ref e) if xml::local_name(e.name().as_ref()) == b"ln" => break,
            Event::Eof => return Err(xml::unexpected_eof(b"ln")),
            _ => {}
        }
    }

    Ok(Outline {
        width,
        cap,
        compound,
        alignment,
        fill,
        dash,
        join,
        head_end,
        tail_end,
    })
}

// ── Children ────────────────────────────────────────────────────────────────

fn parse_custom_dash_stops(reader: &mut Reader<&[u8]>, buf: &mut Vec<u8>) -> Result<Vec<DashStop>> {
    let mut stops = Vec::new();
    loop {
        match xml::next_event(reader, buf)? {
            Event::Empty(ref e) | Event::Start(ref e)
                if xml::local_name(e.name().as_ref()) == b"ds" =>
            {
                let dash = xml::optional_attr_i64(e, b"d")?.ok_or_else(|| {
                    ParseError::MissingAttribute {
                        element: "a:ds".into(),
                        attr: "d".into(),
                    }
                })?;
                let space = xml::optional_attr_i64(e, b"sp")?.ok_or_else(|| {
                    ParseError::MissingAttribute {
                        element: "a:ds".into(),
                        attr: "sp".into(),
                    }
                })?;
                stops.push(DashStop {
                    dash: Dimension::new(dash),
                    space: Dimension::new(space),
                });
                // If it was Start, skip to its End.
                // We don't track Start vs Empty here directly; the next event
                // handler (End pattern) will close out naturally.
            }
            Event::End(ref e) if xml::local_name(e.name().as_ref()) == b"custDash" => break,
            Event::Eof => return Err(xml::unexpected_eof(b"custDash")),
            _ => {}
        }
    }
    Ok(stops)
}

fn parse_line_end(e: &BytesStart<'_>) -> Result<LineEnd> {
    let kind = xml::optional_attr(e, b"type")?
        .map(|s| parse_line_end_type(&s))
        .transpose()?
        .unwrap_or(LineEndType::None);
    let width = xml::optional_attr(e, b"w")?
        .map(|s| parse_line_end_size(&s, "w"))
        .transpose()?
        .unwrap_or(LineEndSize::Med);
    let length = xml::optional_attr(e, b"len")?
        .map(|s| parse_line_end_size(&s, "len"))
        .transpose()?
        .unwrap_or(LineEndSize::Med);
    Ok(LineEnd {
        kind,
        width,
        length,
    })
}

// ── Enumerated attribute parsers ────────────────────────────────────────────

fn parse_line_cap(val: &str) -> Result<LineCap> {
    match val {
        "flat" => Ok(LineCap::Flat),
        "rnd" => Ok(LineCap::Round),
        "sq" => Ok(LineCap::Square),
        other => Err(ParseError::InvalidAttributeValue {
            attr: "cap".into(),
            value: other.into(),
            reason: "expected flat, rnd, or sq per §20.1.10.31 ST_LineCap".into(),
        }),
    }
}

fn parse_compound_line(val: &str) -> Result<CompoundLine> {
    match val {
        "sng" => Ok(CompoundLine::Single),
        "dbl" => Ok(CompoundLine::Double),
        "thickThin" => Ok(CompoundLine::ThickThin),
        "thinThick" => Ok(CompoundLine::ThinThick),
        "tri" => Ok(CompoundLine::Triple),
        other => Err(ParseError::InvalidAttributeValue {
            attr: "cmpd".into(),
            value: other.into(),
            reason: "expected value per §20.1.10.15 ST_CompoundLine".into(),
        }),
    }
}

fn parse_pen_alignment(val: &str) -> Result<PenAlignment> {
    match val {
        "ctr" => Ok(PenAlignment::Center),
        "in" => Ok(PenAlignment::Inset),
        other => Err(ParseError::InvalidAttributeValue {
            attr: "algn".into(),
            value: other.into(),
            reason: "expected ctr or in per §20.1.10.39 ST_PenAlignment".into(),
        }),
    }
}

fn parse_preset_line_dash_val(val: &str) -> Result<PresetLineDashVal> {
    match val {
        "solid" => Ok(PresetLineDashVal::Solid),
        "dot" => Ok(PresetLineDashVal::Dot),
        "dash" => Ok(PresetLineDashVal::Dash),
        "lgDash" => Ok(PresetLineDashVal::LgDash),
        "dashDot" => Ok(PresetLineDashVal::DashDot),
        "lgDashDot" => Ok(PresetLineDashVal::LgDashDot),
        "lgDashDotDot" => Ok(PresetLineDashVal::LgDashDotDot),
        "sysDash" => Ok(PresetLineDashVal::SysDash),
        "sysDot" => Ok(PresetLineDashVal::SysDot),
        "sysDashDot" => Ok(PresetLineDashVal::SysDashDot),
        "sysDashDotDot" => Ok(PresetLineDashVal::SysDashDotDot),
        other => Err(ParseError::InvalidAttributeValue {
            attr: "val".into(),
            value: other.into(),
            reason: "expected value per §20.1.10.48 ST_PresetLineDashVal".into(),
        }),
    }
}

fn parse_line_end_type(val: &str) -> Result<LineEndType> {
    match val {
        "none" => Ok(LineEndType::None),
        "triangle" => Ok(LineEndType::Triangle),
        "stealth" => Ok(LineEndType::Stealth),
        "diamond" => Ok(LineEndType::Diamond),
        "oval" => Ok(LineEndType::Oval),
        "arrow" => Ok(LineEndType::Arrow),
        other => Err(ParseError::InvalidAttributeValue {
            attr: "type".into(),
            value: other.into(),
            reason: "expected value per §20.1.10.33 ST_LineEndType".into(),
        }),
    }
}

fn parse_line_end_size(val: &str, attr: &str) -> Result<LineEndSize> {
    match val {
        "sm" => Ok(LineEndSize::Sm),
        "med" => Ok(LineEndSize::Med),
        "lg" => Ok(LineEndSize::Lg),
        other => Err(ParseError::InvalidAttributeValue {
            attr: attr.into(),
            value: other.into(),
            reason: "expected sm, med, or lg per §20.1.10.34 / §20.1.10.35".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docx::model::DrawingColor;

    fn parse(xml_src: &str) -> Outline {
        let mut reader = Reader::from_reader(xml_src.as_bytes());
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        loop {
            match xml::next_event(&mut reader, &mut buf).unwrap() {
                Event::Start(ref e) if xml::local_name(e.name().as_ref()) == b"ln" => {
                    return parse_outline(&mut reader, &mut buf, e).unwrap();
                }
                Event::Eof => panic!("no ln element"),
                _ => {}
            }
        }
    }

    #[test]
    fn basic_width_and_cap() {
        let o = parse(r#"<a:ln xmlns:a="urn:a" w="12700" cap="rnd"></a:ln>"#);
        assert_eq!(o.width.unwrap().raw(), 12700);
        assert_eq!(o.cap, Some(LineCap::Round));
    }

    #[test]
    fn solid_fill_color() {
        let o = parse(
            r#"<a:ln xmlns:a="urn:a" w="9525">
                <a:solidFill>
                    <a:srgbClr val="D99F34"/>
                </a:solidFill>
            </a:ln>"#,
        );
        assert!(matches!(
            o.fill,
            Some(DrawingFill::Solid(DrawingColor::Srgb { rgb: 0xD99F34, .. }))
        ));
    }

    #[test]
    fn preset_dash() {
        let o = parse(r#"<a:ln xmlns:a="urn:a"><a:prstDash val="dashDot"/></a:ln>"#);
        assert!(matches!(
            o.dash,
            Some(LineDash::Preset(PresetLineDashVal::DashDot))
        ));
    }

    #[test]
    fn custom_dash_stops() {
        let o = parse(
            r#"<a:ln xmlns:a="urn:a">
                <a:custDash>
                    <a:ds d="100000" sp="50000"/>
                    <a:ds d="200000" sp="100000"/>
                </a:custDash>
            </a:ln>"#,
        );
        let Some(LineDash::Custom(stops)) = o.dash else {
            panic!()
        };
        assert_eq!(stops.len(), 2);
        assert_eq!(stops[0].dash.raw(), 100_000);
        assert_eq!(stops[0].space.raw(), 50_000);
        assert_eq!(stops[1].dash.raw(), 200_000);
    }

    #[test]
    fn miter_join_with_limit() {
        let o = parse(r#"<a:ln xmlns:a="urn:a"><a:miter lim="800000"/></a:ln>"#);
        let Some(LineJoin::Miter { limit }) = o.join else {
            panic!()
        };
        assert_eq!(limit.unwrap().raw(), 800_000);
    }

    #[test]
    fn round_join() {
        let o = parse(r#"<a:ln xmlns:a="urn:a"><a:round/></a:ln>"#);
        assert_eq!(o.join, Some(LineJoin::Round));
    }

    #[test]
    fn bevel_join() {
        let o = parse(r#"<a:ln xmlns:a="urn:a"><a:bevel/></a:ln>"#);
        assert_eq!(o.join, Some(LineJoin::Bevel));
    }

    #[test]
    fn head_and_tail_ends() {
        let o = parse(
            r#"<a:ln xmlns:a="urn:a">
                <a:headEnd type="triangle" w="med" len="lg"/>
                <a:tailEnd type="stealth" w="sm" len="sm"/>
            </a:ln>"#,
        );
        assert_eq!(
            o.head_end,
            Some(LineEnd {
                kind: LineEndType::Triangle,
                width: LineEndSize::Med,
                length: LineEndSize::Lg,
            })
        );
        assert_eq!(
            o.tail_end,
            Some(LineEnd {
                kind: LineEndType::Stealth,
                width: LineEndSize::Sm,
                length: LineEndSize::Sm,
            })
        );
    }

    #[test]
    fn compound_line_types() {
        for (val, expected) in [
            ("sng", CompoundLine::Single),
            ("dbl", CompoundLine::Double),
            ("thickThin", CompoundLine::ThickThin),
            ("thinThick", CompoundLine::ThinThick),
            ("tri", CompoundLine::Triple),
        ] {
            let o = parse(&format!(r#"<a:ln xmlns:a="urn:a" cmpd="{val}"></a:ln>"#));
            assert_eq!(o.compound, Some(expected));
        }
    }

    #[test]
    fn invalid_line_cap_errors() {
        let mut reader =
            Reader::from_reader(r#"<a:ln xmlns:a="urn:a" cap="bogus"></a:ln>"#.as_bytes());
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        loop {
            match xml::next_event(&mut reader, &mut buf).unwrap() {
                Event::Start(ref e) if xml::local_name(e.name().as_ref()) == b"ln" => {
                    assert!(parse_outline(&mut reader, &mut buf, e).is_err());
                    return;
                }
                Event::Eof => panic!(),
                _ => {}
            }
        }
    }

    #[test]
    fn invalid_preset_dash_errors() {
        let mut reader = Reader::from_reader(
            r#"<a:ln xmlns:a="urn:a"><a:prstDash val="bogus"/></a:ln>"#.as_bytes(),
        );
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        loop {
            match xml::next_event(&mut reader, &mut buf).unwrap() {
                Event::Start(ref e) if xml::local_name(e.name().as_ref()) == b"ln" => {
                    assert!(parse_outline(&mut reader, &mut buf, e).is_err());
                    return;
                }
                Event::Eof => panic!(),
                _ => {}
            }
        }
    }

    #[test]
    fn pen_alignment() {
        let o = parse(r#"<a:ln xmlns:a="urn:a" algn="ctr"></a:ln>"#);
        assert_eq!(o.alignment, Some(PenAlignment::Center));
        let o = parse(r#"<a:ln xmlns:a="urn:a" algn="in"></a:ln>"#);
        assert_eq!(o.alignment, Some(PenAlignment::Inset));
    }
}
