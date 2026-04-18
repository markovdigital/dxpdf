//! Parser for DrawingML fill choice elements (§20.1.8 EG_FillProperties).
//!
//! Dispatches on the six choice elements (`noFill`, `solidFill`, `gradFill`,
//! `blipFill`, `pattFill`, `grpFill`). Each internal parser is a pure
//! function over `(reader, buf, start)` returning a typed model value.

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use crate::docx::dimension::Dimension;
use crate::docx::error::{ParseError, Result};
use crate::docx::model::{
    Blip, BlipCompression, BlipFill, BlipFillKind, DrawingColor, DrawingFill, GradientFill,
    GradientShadeProperties, GradientStop, PathShadeType, PatternFill, PresetPatternVal,
    RectAlignment, RelativeRect, StretchFill, TileFill, TileFlipMode,
};
use crate::docx::xml;

use super::color::parse_color_choice;

/// Dispatch a single `EG_FillProperties` choice element. Reader is positioned
/// at the choice's Start or Empty event; caller supplies the event and its
/// `is_empty` flag.
///
/// Returns `Ok(None)` for elements outside the recognized choice set; caller
/// logs and advances.
pub fn parse_drawing_fill(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    start: &BytesStart<'_>,
    is_empty: bool,
) -> Result<Option<DrawingFill>> {
    let qn = start.name();
    let local = xml::local_name(qn.as_ref());
    match local {
        b"noFill" => {
            if !is_empty {
                xml::skip_to_end(reader, buf, b"noFill")?;
            }
            Ok(Some(DrawingFill::None))
        }
        b"grpFill" => {
            if !is_empty {
                xml::skip_to_end(reader, buf, b"grpFill")?;
            }
            Ok(Some(DrawingFill::Group))
        }
        b"solidFill" => {
            let color = parse_solid_fill(reader, buf, is_empty)?;
            Ok(color.map(DrawingFill::Solid))
        }
        b"gradFill" => Ok(Some(DrawingFill::Gradient(parse_gradient_fill(
            reader, buf, start, is_empty,
        )?))),
        b"blipFill" => Ok(Some(DrawingFill::Blip(parse_blip_fill(
            reader, buf, start, is_empty,
        )?))),
        b"pattFill" => Ok(Some(DrawingFill::Pattern(parse_pattern_fill(
            reader, buf, start, is_empty,
        )?))),
        _ => Ok(None),
    }
}

// ── solidFill (§20.1.8.54) ──────────────────────────────────────────────────

fn parse_solid_fill(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    is_empty: bool,
) -> Result<Option<DrawingColor>> {
    if is_empty {
        // A solidFill without a color child is ill-formed per spec but we
        // treat it as absent rather than panic; the caller will see None.
        return Ok(None);
    }
    let mut color: Option<DrawingColor> = None;
    loop {
        match xml::next_event(reader, buf)? {
            Event::Start(ref e) => {
                let local = xml::local_name_owned(e.name().as_ref());
                if let Some(c) = parse_color_choice(reader, buf, e, false)? {
                    color = Some(c);
                } else {
                    xml::warn_unsupported_element("solidFill", &local);
                    xml::skip_to_end(reader, buf, &local)?;
                }
            }
            Event::Empty(ref e) => {
                let local = xml::local_name_owned(e.name().as_ref());
                if let Some(c) = parse_color_choice(reader, buf, e, true)? {
                    color = Some(c);
                } else {
                    xml::warn_unsupported_element("solidFill", &local);
                }
            }
            Event::End(ref e) if xml::local_name(e.name().as_ref()) == b"solidFill" => break,
            Event::Eof => return Err(xml::unexpected_eof(b"solidFill")),
            _ => {}
        }
    }
    Ok(color)
}

// ── gradFill (§20.1.8.33) ───────────────────────────────────────────────────

fn parse_gradient_fill(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    start: &BytesStart<'_>,
    is_empty: bool,
) -> Result<GradientFill> {
    let flip = xml::optional_attr(start, b"flip")?
        .map(|s| parse_tile_flip_mode(&s))
        .transpose()?;
    let rot_with_shape = xml::optional_attr_bool(start, b"rotWithShape")?;
    let mut stops = Vec::new();
    let mut shade_properties: Option<GradientShadeProperties> = None;
    let mut tile_rect: Option<RelativeRect> = None;

    if is_empty {
        return Ok(GradientFill {
            stops,
            shade_properties: GradientShadeProperties::Linear {
                angle: Dimension::new(0),
                scaled: None,
            },
            flip,
            rot_with_shape,
            tile_rect,
        });
    }

    loop {
        match xml::next_event(reader, buf)? {
            Event::Start(ref e) => {
                let local = xml::local_name_owned(e.name().as_ref());
                match &*local {
                    b"gsLst" => stops = parse_gradient_stop_list(reader, buf)?,
                    b"lin" => {
                        shade_properties = Some(parse_linear_shade(e)?);
                        xml::skip_to_end(reader, buf, b"lin")?;
                    }
                    b"path" => {
                        shade_properties = Some(parse_path_shade(reader, buf, e)?);
                    }
                    b"tileRect" => {
                        tile_rect = Some(parse_relative_rect(e)?);
                        xml::skip_to_end(reader, buf, b"tileRect")?;
                    }
                    _ => {
                        xml::warn_unsupported_element("gradFill", &local);
                        xml::skip_to_end(reader, buf, &local)?;
                    }
                }
            }
            Event::Empty(ref e) => {
                let qn = e.name();
                let local = xml::local_name(qn.as_ref());
                match local {
                    b"lin" => shade_properties = Some(parse_linear_shade(e)?),
                    b"path" => shade_properties = Some(parse_path_shade_attrs_only(e)?),
                    b"tileRect" => tile_rect = Some(parse_relative_rect(e)?),
                    _ => xml::warn_unsupported_element("gradFill", local),
                }
            }
            Event::End(ref e) if xml::local_name(e.name().as_ref()) == b"gradFill" => break,
            Event::Eof => return Err(xml::unexpected_eof(b"gradFill")),
            _ => {}
        }
    }

    // Default shade when neither lin nor path is specified: linear at 0°.
    let shade_properties = shade_properties.unwrap_or(GradientShadeProperties::Linear {
        angle: Dimension::new(0),
        scaled: None,
    });

    Ok(GradientFill {
        stops,
        shade_properties,
        flip,
        rot_with_shape,
        tile_rect,
    })
}

fn parse_gradient_stop_list(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
) -> Result<Vec<GradientStop>> {
    let mut stops = Vec::new();
    loop {
        match xml::next_event(reader, buf)? {
            Event::Start(ref e) if xml::local_name(e.name().as_ref()) == b"gs" => {
                stops.push(parse_gradient_stop(reader, buf, e, false)?);
            }
            Event::Empty(ref e) if xml::local_name(e.name().as_ref()) == b"gs" => {
                stops.push(parse_gradient_stop(reader, buf, e, true)?);
            }
            Event::End(ref e) if xml::local_name(e.name().as_ref()) == b"gsLst" => break,
            Event::Eof => return Err(xml::unexpected_eof(b"gsLst")),
            _ => {}
        }
    }
    Ok(stops)
}

fn parse_gradient_stop(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    start: &BytesStart<'_>,
    is_empty: bool,
) -> Result<GradientStop> {
    let pos =
        xml::optional_attr_i64(start, b"pos")?.ok_or_else(|| ParseError::MissingAttribute {
            element: "a:gs".into(),
            attr: "pos".into(),
        })?;
    let mut color: Option<DrawingColor> = None;

    if !is_empty {
        loop {
            match xml::next_event(reader, buf)? {
                Event::Start(ref e) => {
                    let local = xml::local_name_owned(e.name().as_ref());
                    if let Some(c) = parse_color_choice(reader, buf, e, false)? {
                        color = Some(c);
                    } else {
                        xml::warn_unsupported_element("gs", &local);
                        xml::skip_to_end(reader, buf, &local)?;
                    }
                }
                Event::Empty(ref e) => {
                    let local = xml::local_name_owned(e.name().as_ref());
                    if let Some(c) = parse_color_choice(reader, buf, e, true)? {
                        color = Some(c);
                    } else {
                        xml::warn_unsupported_element("gs", &local);
                    }
                }
                Event::End(ref e) if xml::local_name(e.name().as_ref()) == b"gs" => break,
                Event::Eof => return Err(xml::unexpected_eof(b"gs")),
                _ => {}
            }
        }
    }

    let color = color.ok_or_else(|| ParseError::MissingElement {
        parent: "a:gs".into(),
        child: "color".into(),
    })?;

    Ok(GradientStop {
        position: Dimension::new(pos),
        color,
    })
}

fn parse_linear_shade(e: &BytesStart<'_>) -> Result<GradientShadeProperties> {
    let angle = xml::optional_attr_i64(e, b"ang")?.unwrap_or(0);
    let scaled = xml::optional_attr_bool(e, b"scaled")?;
    Ok(GradientShadeProperties::Linear {
        angle: Dimension::new(angle),
        scaled,
    })
}

fn parse_path_shade(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    start: &BytesStart<'_>,
) -> Result<GradientShadeProperties> {
    let path_type =
        parse_path_shade_type(&xml::optional_attr(start, b"path")?.ok_or_else(|| {
            ParseError::MissingAttribute {
                element: "a:path".into(),
                attr: "path".into(),
            }
        })?)?;
    let mut fill_to_rect: Option<RelativeRect> = None;
    loop {
        match xml::next_event(reader, buf)? {
            Event::Empty(ref e) if xml::local_name(e.name().as_ref()) == b"fillToRect" => {
                fill_to_rect = Some(parse_relative_rect(e)?);
            }
            Event::Start(ref e) if xml::local_name(e.name().as_ref()) == b"fillToRect" => {
                fill_to_rect = Some(parse_relative_rect(e)?);
                xml::skip_to_end(reader, buf, b"fillToRect")?;
            }
            Event::End(ref e) if xml::local_name(e.name().as_ref()) == b"path" => break,
            Event::Eof => return Err(xml::unexpected_eof(b"path")),
            _ => {}
        }
    }
    Ok(GradientShadeProperties::Path {
        path_type,
        fill_to_rect,
    })
}

fn parse_path_shade_attrs_only(e: &BytesStart<'_>) -> Result<GradientShadeProperties> {
    let path_type = parse_path_shade_type(&xml::optional_attr(e, b"path")?.ok_or_else(|| {
        ParseError::MissingAttribute {
            element: "a:path".into(),
            attr: "path".into(),
        }
    })?)?;
    Ok(GradientShadeProperties::Path {
        path_type,
        fill_to_rect: None,
    })
}

fn parse_path_shade_type(val: &str) -> Result<PathShadeType> {
    match val {
        "shape" => Ok(PathShadeType::Shape),
        "circle" => Ok(PathShadeType::Circle),
        "rect" => Ok(PathShadeType::Rect),
        other => Err(ParseError::InvalidAttributeValue {
            attr: "path".into(),
            value: other.into(),
            reason: "expected value per §20.1.10.46 ST_PathShadeType".into(),
        }),
    }
}

// ── blipFill (§20.1.8.14) ───────────────────────────────────────────────────

fn parse_blip_fill(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    start: &BytesStart<'_>,
    is_empty: bool,
) -> Result<BlipFill> {
    let rotate_with_shape = xml::optional_attr_bool(start, b"rotWithShape")?;
    let dpi = xml::optional_attr_u32(start, b"dpi")?;
    let mut blip = None;
    let mut src_rect = None;
    let mut fill_kind = BlipFillKind::Unspecified;

    if is_empty {
        return Ok(BlipFill {
            rotate_with_shape,
            dpi,
            blip,
            src_rect,
            fill_kind,
        });
    }

    loop {
        match xml::next_event(reader, buf)? {
            Event::Start(ref e) => {
                let local = xml::local_name_owned(e.name().as_ref());
                match &*local {
                    b"blip" => {
                        blip = Some(parse_blip(reader, buf, e)?);
                    }
                    b"stretch" => {
                        fill_kind = BlipFillKind::Stretch(parse_stretch(reader, buf)?);
                    }
                    b"tile" => {
                        fill_kind = BlipFillKind::Tile(parse_tile_attrs(e)?);
                        xml::skip_to_end(reader, buf, b"tile")?;
                    }
                    b"srcRect" => {
                        src_rect = Some(parse_relative_rect(e)?);
                        xml::skip_to_end(reader, buf, b"srcRect")?;
                    }
                    _ => {
                        xml::warn_unsupported_element("blipFill", &local);
                        xml::skip_to_end(reader, buf, &local)?;
                    }
                }
            }
            Event::Empty(ref e) => {
                let qn = e.name();
                let local = xml::local_name(qn.as_ref());
                match local {
                    b"blip" => blip = Some(parse_blip_attrs(e)?),
                    b"srcRect" => src_rect = Some(parse_relative_rect(e)?),
                    b"tile" => fill_kind = BlipFillKind::Tile(parse_tile_attrs(e)?),
                    _ => xml::warn_unsupported_element("blipFill", local),
                }
            }
            Event::End(ref e) if xml::local_name(e.name().as_ref()) == b"blipFill" => break,
            Event::Eof => return Err(xml::unexpected_eof(b"blipFill")),
            _ => {}
        }
    }

    Ok(BlipFill {
        rotate_with_shape,
        dpi,
        blip,
        src_rect,
        fill_kind,
    })
}

fn parse_blip(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    start: &BytesStart<'_>,
) -> Result<Blip> {
    let blip = parse_blip_attrs(start)?;
    xml::skip_to_end(reader, buf, b"blip")?;
    Ok(blip)
}

fn parse_blip_attrs(e: &BytesStart<'_>) -> Result<Blip> {
    use crate::model::RelId;
    Ok(Blip {
        embed: xml::optional_attr(e, b"embed")?.map(RelId::new),
        link: xml::optional_attr(e, b"link")?.map(RelId::new),
        compression: xml::optional_attr(e, b"cstate")?
            .map(|s| parse_blip_compression(&s))
            .transpose()?,
    })
}

fn parse_blip_compression(val: &str) -> Result<BlipCompression> {
    match val {
        "email" => Ok(BlipCompression::Email),
        "hqprint" => Ok(BlipCompression::Hqprint),
        "none" => Ok(BlipCompression::None),
        "print" => Ok(BlipCompression::Print),
        "screen" => Ok(BlipCompression::Screen),
        other => Err(ParseError::InvalidAttributeValue {
            attr: "cstate".into(),
            value: other.into(),
            reason: "expected value per §20.1.10.7 ST_BlipCompression".into(),
        }),
    }
}

fn parse_stretch(reader: &mut Reader<&[u8]>, buf: &mut Vec<u8>) -> Result<StretchFill> {
    let mut fill_rect = None;
    loop {
        match xml::next_event(reader, buf)? {
            Event::Empty(ref e) => {
                let qn = e.name();
                let local = xml::local_name(qn.as_ref());
                match local {
                    b"fillRect" => fill_rect = Some(parse_relative_rect(e)?),
                    _ => xml::warn_unsupported_element("stretch", local),
                }
            }
            Event::Start(ref e) if xml::local_name(e.name().as_ref()) == b"fillRect" => {
                fill_rect = Some(parse_relative_rect(e)?);
                xml::skip_to_end(reader, buf, b"fillRect")?;
            }
            Event::End(ref e) if xml::local_name(e.name().as_ref()) == b"stretch" => break,
            Event::Eof => return Err(xml::unexpected_eof(b"stretch")),
            _ => {}
        }
    }
    Ok(StretchFill { fill_rect })
}

fn parse_tile_attrs(e: &BytesStart<'_>) -> Result<TileFill> {
    Ok(TileFill {
        tx: xml::optional_attr_i64(e, b"tx")?.map(Dimension::new),
        ty: xml::optional_attr_i64(e, b"ty")?.map(Dimension::new),
        sx: xml::optional_attr_i64(e, b"sx")?.map(Dimension::new),
        sy: xml::optional_attr_i64(e, b"sy")?.map(Dimension::new),
        flip: xml::optional_attr(e, b"flip")?
            .map(|s| parse_tile_flip_mode(&s))
            .transpose()?,
        alignment: xml::optional_attr(e, b"algn")?
            .map(|s| parse_rect_alignment(&s))
            .transpose()?,
    })
}

// ── pattFill (§20.1.8.47) ───────────────────────────────────────────────────

fn parse_pattern_fill(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    start: &BytesStart<'_>,
    is_empty: bool,
) -> Result<PatternFill> {
    let preset =
        parse_preset_pattern_val(&xml::optional_attr(start, b"prst")?.ok_or_else(|| {
            ParseError::MissingAttribute {
                element: "a:pattFill".into(),
                attr: "prst".into(),
            }
        })?)?;
    let mut fg_color: Option<DrawingColor> = None;
    let mut bg_color: Option<DrawingColor> = None;

    if is_empty {
        return Ok(PatternFill {
            preset,
            fg_color,
            bg_color,
        });
    }

    loop {
        match xml::next_event(reader, buf)? {
            Event::Start(ref e) => {
                let local = xml::local_name_owned(e.name().as_ref());
                match &*local {
                    b"fgClr" => {
                        fg_color = parse_color_wrapper(reader, buf, b"fgClr")?;
                    }
                    b"bgClr" => {
                        bg_color = parse_color_wrapper(reader, buf, b"bgClr")?;
                    }
                    _ => {
                        xml::warn_unsupported_element("pattFill", &local);
                        xml::skip_to_end(reader, buf, &local)?;
                    }
                }
            }
            Event::End(ref e) if xml::local_name(e.name().as_ref()) == b"pattFill" => break,
            Event::Eof => return Err(xml::unexpected_eof(b"pattFill")),
            _ => {}
        }
    }

    Ok(PatternFill {
        preset,
        fg_color,
        bg_color,
    })
}

/// Parse a color wrapper element (`fgClr`, `bgClr`) whose single child is a
/// color choice.
fn parse_color_wrapper(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    end_tag: &[u8],
) -> Result<Option<DrawingColor>> {
    let mut color: Option<DrawingColor> = None;
    loop {
        match xml::next_event(reader, buf)? {
            Event::Start(ref e) => {
                let local = xml::local_name_owned(e.name().as_ref());
                if let Some(c) = parse_color_choice(reader, buf, e, false)? {
                    color = Some(c);
                } else {
                    xml::warn_unsupported_element("colorWrapper", &local);
                    xml::skip_to_end(reader, buf, &local)?;
                }
            }
            Event::Empty(ref e) => {
                let local = xml::local_name_owned(e.name().as_ref());
                if let Some(c) = parse_color_choice(reader, buf, e, true)? {
                    color = Some(c);
                } else {
                    xml::warn_unsupported_element("colorWrapper", &local);
                }
            }
            Event::End(ref e) if xml::local_name(e.name().as_ref()) == end_tag => break,
            Event::Eof => return Err(xml::unexpected_eof(end_tag)),
            _ => {}
        }
    }
    Ok(color)
}

// ── Shared attribute parsers ────────────────────────────────────────────────

fn parse_relative_rect(e: &BytesStart<'_>) -> Result<RelativeRect> {
    Ok(RelativeRect {
        left: xml::optional_attr_i64(e, b"l")?.map(Dimension::new),
        top: xml::optional_attr_i64(e, b"t")?.map(Dimension::new),
        right: xml::optional_attr_i64(e, b"r")?.map(Dimension::new),
        bottom: xml::optional_attr_i64(e, b"b")?.map(Dimension::new),
    })
}

fn parse_tile_flip_mode(val: &str) -> Result<TileFlipMode> {
    match val {
        "none" => Ok(TileFlipMode::None),
        "x" => Ok(TileFlipMode::X),
        "y" => Ok(TileFlipMode::Y),
        "xy" => Ok(TileFlipMode::Xy),
        other => Err(ParseError::InvalidAttributeValue {
            attr: "flip".into(),
            value: other.into(),
            reason: "expected value per §20.1.10.86 ST_TileFlipMode".into(),
        }),
    }
}

pub(super) fn parse_rect_alignment(val: &str) -> Result<RectAlignment> {
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

fn parse_preset_pattern_val(val: &str) -> Result<PresetPatternVal> {
    Ok(match val {
        "pct5" => PresetPatternVal::Pct5,
        "pct10" => PresetPatternVal::Pct10,
        "pct20" => PresetPatternVal::Pct20,
        "pct25" => PresetPatternVal::Pct25,
        "pct30" => PresetPatternVal::Pct30,
        "pct40" => PresetPatternVal::Pct40,
        "pct50" => PresetPatternVal::Pct50,
        "pct60" => PresetPatternVal::Pct60,
        "pct70" => PresetPatternVal::Pct70,
        "pct75" => PresetPatternVal::Pct75,
        "pct80" => PresetPatternVal::Pct80,
        "pct90" => PresetPatternVal::Pct90,
        "horz" => PresetPatternVal::Horz,
        "vert" => PresetPatternVal::Vert,
        "ltHorz" => PresetPatternVal::LtHorz,
        "ltVert" => PresetPatternVal::LtVert,
        "dkHorz" => PresetPatternVal::DkHorz,
        "dkVert" => PresetPatternVal::DkVert,
        "narHorz" => PresetPatternVal::NarHorz,
        "narVert" => PresetPatternVal::NarVert,
        "dashHorz" => PresetPatternVal::DashHorz,
        "dashVert" => PresetPatternVal::DashVert,
        "cross" => PresetPatternVal::Cross,
        "dnDiag" => PresetPatternVal::DnDiag,
        "upDiag" => PresetPatternVal::UpDiag,
        "ltDnDiag" => PresetPatternVal::LtDnDiag,
        "ltUpDiag" => PresetPatternVal::LtUpDiag,
        "dkDnDiag" => PresetPatternVal::DkDnDiag,
        "dkUpDiag" => PresetPatternVal::DkUpDiag,
        "wdDnDiag" => PresetPatternVal::WdDnDiag,
        "wdUpDiag" => PresetPatternVal::WdUpDiag,
        "dashDnDiag" => PresetPatternVal::DashDnDiag,
        "dashUpDiag" => PresetPatternVal::DashUpDiag,
        "diagCross" => PresetPatternVal::DiagCross,
        "smCheck" => PresetPatternVal::SmCheck,
        "lgCheck" => PresetPatternVal::LgCheck,
        "smGrid" => PresetPatternVal::SmGrid,
        "lgGrid" => PresetPatternVal::LgGrid,
        "dotGrid" => PresetPatternVal::DotGrid,
        "smConfetti" => PresetPatternVal::SmConfetti,
        "lgConfetti" => PresetPatternVal::LgConfetti,
        "horzBrick" => PresetPatternVal::HorzBrick,
        "diagBrick" => PresetPatternVal::DiagBrick,
        "solidDmnd" => PresetPatternVal::SolidDmnd,
        "openDmnd" => PresetPatternVal::OpenDmnd,
        "dotDmnd" => PresetPatternVal::DotDmnd,
        "plaid" => PresetPatternVal::Plaid,
        "sphere" => PresetPatternVal::Sphere,
        "weave" => PresetPatternVal::Weave,
        "divotShingle" => PresetPatternVal::DivotShingle,
        "trellis" => PresetPatternVal::Trellis,
        "zigZag" => PresetPatternVal::ZigZag,
        "wave" => PresetPatternVal::Wave,
        other => {
            return Err(ParseError::InvalidAttributeValue {
                attr: "prst".into(),
                value: other.into(),
                reason: "expected value per §20.1.10.50 ST_PresetPatternVal".into(),
            });
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use quick_xml::Reader;

    fn parse(xml_src: &str) -> DrawingFill {
        let mut reader = Reader::from_reader(xml_src.as_bytes());
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        loop {
            match xml::next_event(&mut reader, &mut buf).unwrap() {
                Event::Start(ref e) => {
                    return parse_drawing_fill(&mut reader, &mut buf, e, false)
                        .unwrap()
                        .unwrap()
                }
                Event::Empty(ref e) => {
                    return parse_drawing_fill(&mut reader, &mut buf, e, true)
                        .unwrap()
                        .unwrap()
                }
                Event::Eof => panic!("no fill element"),
                _ => {}
            }
        }
    }

    #[test]
    fn no_fill_empty() {
        assert!(matches!(
            parse(r#"<a:noFill xmlns:a="urn:a"/>"#),
            DrawingFill::None
        ));
    }

    #[test]
    fn grp_fill_empty() {
        assert!(matches!(
            parse(r#"<a:grpFill xmlns:a="urn:a"/>"#),
            DrawingFill::Group
        ));
    }

    #[test]
    fn solid_fill_srgb() {
        let f = parse(
            r#"<a:solidFill xmlns:a="urn:a">
                <a:srgbClr val="FF0000"/>
            </a:solidFill>"#,
        );
        assert!(matches!(
            f,
            DrawingFill::Solid(DrawingColor::Srgb { rgb: 0xFF0000, .. })
        ));
    }

    #[test]
    fn solid_fill_scheme_with_transform() {
        let f = parse(
            r#"<a:solidFill xmlns:a="urn:a">
                <a:schemeClr val="accent1">
                    <a:lumMod val="75000"/>
                </a:schemeClr>
            </a:solidFill>"#,
        );
        let DrawingFill::Solid(color) = f else {
            panic!()
        };
        assert_eq!(color.transforms().len(), 1);
    }

    #[test]
    fn gradient_fill_linear_two_stops() {
        let f = parse(
            r#"<a:gradFill xmlns:a="urn:a" flip="none" rotWithShape="1">
                <a:gsLst>
                    <a:gs pos="0">
                        <a:srgbClr val="000000"/>
                    </a:gs>
                    <a:gs pos="100000">
                        <a:srgbClr val="FFFFFF"/>
                    </a:gs>
                </a:gsLst>
                <a:lin ang="5400000" scaled="1"/>
            </a:gradFill>"#,
        );
        let DrawingFill::Gradient(g) = f else {
            panic!()
        };
        assert_eq!(g.stops.len(), 2);
        assert_eq!(g.stops[0].position.raw(), 0);
        assert_eq!(g.stops[1].position.raw(), 100_000);
        assert!(matches!(
            g.shade_properties,
            GradientShadeProperties::Linear { .. }
        ));
        assert_eq!(g.rot_with_shape, Some(true));
        assert_eq!(g.flip, Some(TileFlipMode::None));
    }

    #[test]
    fn gradient_fill_path_circle() {
        let f = parse(
            r#"<a:gradFill xmlns:a="urn:a">
                <a:gsLst>
                    <a:gs pos="50000"><a:srgbClr val="808080"/></a:gs>
                </a:gsLst>
                <a:path path="circle">
                    <a:fillToRect l="50000" t="50000" r="50000" b="50000"/>
                </a:path>
            </a:gradFill>"#,
        );
        let DrawingFill::Gradient(g) = f else {
            panic!()
        };
        match g.shade_properties {
            GradientShadeProperties::Path {
                path_type: PathShadeType::Circle,
                fill_to_rect: Some(_),
            } => {}
            other => panic!("expected path/circle with fill_to_rect, got {other:?}"),
        }
    }

    #[test]
    fn gradient_stop_order_preserved() {
        let f = parse(
            r#"<a:gradFill xmlns:a="urn:a">
                <a:gsLst>
                    <a:gs pos="0"><a:srgbClr val="FF0000"/></a:gs>
                    <a:gs pos="50000"><a:srgbClr val="00FF00"/></a:gs>
                    <a:gs pos="100000"><a:srgbClr val="0000FF"/></a:gs>
                </a:gsLst>
                <a:lin ang="0" scaled="0"/>
            </a:gradFill>"#,
        );
        let DrawingFill::Gradient(g) = f else {
            panic!()
        };
        assert_eq!(g.stops.len(), 3);
        assert!(matches!(
            g.stops[0].color,
            DrawingColor::Srgb { rgb: 0xFF0000, .. }
        ));
        assert!(matches!(
            g.stops[1].color,
            DrawingColor::Srgb { rgb: 0x00FF00, .. }
        ));
        assert!(matches!(
            g.stops[2].color,
            DrawingColor::Srgb { rgb: 0x0000FF, .. }
        ));
    }

    #[test]
    fn blip_fill_stretch() {
        let f = parse(
            r#"<a:blipFill xmlns:a="urn:a" dpi="96" rotWithShape="1">
                <a:blip r:embed="rId7" xmlns:r="urn:r"/>
                <a:srcRect l="1000" t="2000"/>
                <a:stretch>
                    <a:fillRect l="0" t="0" r="0" b="0"/>
                </a:stretch>
            </a:blipFill>"#,
        );
        let DrawingFill::Blip(bf) = f else { panic!() };
        assert_eq!(bf.dpi, Some(96));
        assert_eq!(bf.rotate_with_shape, Some(true));
        assert!(bf.blip.is_some());
        assert!(bf.src_rect.is_some());
        assert!(matches!(bf.fill_kind, BlipFillKind::Stretch(_)));
    }

    #[test]
    fn blip_fill_tile() {
        let f = parse(
            r#"<a:blipFill xmlns:a="urn:a">
                <a:blip r:embed="rId9" xmlns:r="urn:r"/>
                <a:tile tx="0" ty="0" sx="100000" sy="100000" flip="x" algn="ctr"/>
            </a:blipFill>"#,
        );
        let DrawingFill::Blip(bf) = f else { panic!() };
        let BlipFillKind::Tile(t) = bf.fill_kind else {
            panic!("expected Tile, got {:?}", bf.fill_kind)
        };
        assert_eq!(t.flip, Some(TileFlipMode::X));
        assert_eq!(t.alignment, Some(RectAlignment::Ctr));
    }

    #[test]
    fn blip_fill_unspecified_when_no_child_mode() {
        let f = parse(
            r#"<a:blipFill xmlns:a="urn:a">
                <a:blip r:embed="rId1" xmlns:r="urn:r"/>
            </a:blipFill>"#,
        );
        let DrawingFill::Blip(bf) = f else { panic!() };
        assert!(matches!(bf.fill_kind, BlipFillKind::Unspecified));
    }

    #[test]
    fn pattern_fill_with_fg_bg() {
        let f = parse(
            r#"<a:pattFill xmlns:a="urn:a" prst="diagCross">
                <a:fgClr><a:srgbClr val="112233"/></a:fgClr>
                <a:bgClr><a:srgbClr val="EEDDCC"/></a:bgClr>
            </a:pattFill>"#,
        );
        let DrawingFill::Pattern(p) = f else { panic!() };
        assert_eq!(p.preset, PresetPatternVal::DiagCross);
        assert!(matches!(
            p.fg_color,
            Some(DrawingColor::Srgb { rgb: 0x112233, .. })
        ));
        assert!(matches!(
            p.bg_color,
            Some(DrawingColor::Srgb { rgb: 0xEEDDCC, .. })
        ));
    }

    #[test]
    fn pattern_fill_without_colors() {
        let f = parse(r#"<a:pattFill xmlns:a="urn:a" prst="pct50"/>"#);
        let DrawingFill::Pattern(p) = f else { panic!() };
        assert_eq!(p.preset, PresetPatternVal::Pct50);
        assert!(p.fg_color.is_none());
        assert!(p.bg_color.is_none());
    }

    #[test]
    fn invalid_preset_pattern_errors() {
        let mut reader =
            Reader::from_reader(r#"<a:pattFill xmlns:a="urn:a" prst="bogus"/>"#.as_bytes());
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        loop {
            match xml::next_event(&mut reader, &mut buf).unwrap() {
                Event::Empty(ref e) => {
                    assert!(parse_drawing_fill(&mut reader, &mut buf, e, true).is_err());
                    return;
                }
                Event::Eof => panic!(),
                _ => {}
            }
        }
    }

    #[test]
    fn unknown_fill_returns_none() {
        let mut reader = Reader::from_reader(r#"<a:weirdFill xmlns:a="urn:a"/>"#.as_bytes());
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        loop {
            match xml::next_event(&mut reader, &mut buf).unwrap() {
                Event::Empty(ref e) => {
                    assert!(parse_drawing_fill(&mut reader, &mut buf, e, true)
                        .unwrap()
                        .is_none());
                    return;
                }
                Event::Eof => panic!(),
                _ => {}
            }
        }
    }
}
