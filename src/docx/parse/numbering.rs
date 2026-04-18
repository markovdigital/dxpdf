//! Parser for `word/numbering.xml` — parses definitions as-is, no resolution.
//!
//! Two-pass approach:
//! 1. `extract_pic_bullet_picts` — event-driven scan that pairs each
//!    `<w:numPicBullet w:numPicBulletId="N">` with its inner `<w:pict>`,
//!    parsed via the existing VML parser.
//! 2. `from_xml::<NumberingXml>` — serde pass over the rest of the document.
//!
//! Step 2's output is merged with step 1's pict map to produce the final
//! `NumberingDefinitions`. VML parsing stays untouched until Phase 6.

use std::collections::HashMap;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::docx::error::Result;
use crate::docx::model::{
    AbstractNumId, AbstractNumbering, Alignment, Indentation, NumId, NumPicBullet,
    NumPicBulletId, NumberFormat, NumberingDefinitions, NumberingInstance,
    NumberingLevelDefinition, Pict, RunProperties,
};
use crate::docx::parse::primitives::st_enums::{StJc, StNumberFormat};
use crate::docx::parse::properties::schema::paragraph::PPrXml;
use crate::docx::parse::properties::schema::run::RPrXml;
use crate::docx::parse::serde_xml::from_xml;
use crate::docx::parse::vml;
use crate::docx::xml;

pub fn parse_numbering(data: &[u8]) -> Result<NumberingDefinitions> {
    if data.is_empty() {
        return Ok(NumberingDefinitions::default());
    }
    let pict_map = extract_pic_bullet_picts(data)?;
    let schema: NumberingXml = from_xml(data)?;
    let mut defs: NumberingDefinitions = schema.into();
    for (id, pict) in pict_map {
        if let Some(bullet) = defs.pic_bullets.get_mut(&id) {
            bullet.pict = Some(pict);
        }
    }
    Ok(defs)
}

/// Scan raw XML for `<w:numPicBullet w:numPicBulletId="N">…<w:pict>…</w:pict>…</w:numPicBullet>`
/// and parse each inner pict via the legacy VML parser. Returns `{ id → Pict }`.
fn extract_pic_bullet_picts(data: &[u8]) -> Result<HashMap<NumPicBulletId, Pict>> {
    let mut reader = Reader::from_reader(data);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut out = HashMap::new();

    loop {
        match xml::next_event(&mut reader, &mut buf)? {
            Event::Start(ref e) if xml::local_name(e.name().as_ref()) == b"numPicBullet" => {
                let Some(id) = xml::optional_attr_i64(e, b"numPicBulletId")? else {
                    xml::skip_to_end(&mut reader, &mut buf, b"numPicBullet")?;
                    continue;
                };
                if let Some(pict) = find_pict_within(&mut reader, &mut buf)? {
                    out.insert(NumPicBulletId::new(id), pict);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// Inside a `<w:numPicBullet>`, consume events until `</w:numPicBullet>`.
/// If a `<w:pict>` Start is seen, hand off to `vml::parse_pict`.
fn find_pict_within(reader: &mut Reader<&[u8]>, buf: &mut Vec<u8>) -> Result<Option<Pict>> {
    let mut found: Option<Pict> = None;
    loop {
        match xml::next_event(reader, buf)? {
            Event::Start(ref e) => {
                let name = e.name();
                let local_owned: Vec<u8> = xml::local_name(name.as_ref()).to_vec();
                if local_owned == b"pict" && found.is_none() {
                    found = Some(vml::parse_pict(reader, buf)?);
                } else {
                    xml::skip_to_end(reader, buf, &local_owned)?;
                }
            }
            Event::End(ref e) if xml::local_name(e.name().as_ref()) == b"numPicBullet" => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(found)
}

// ── serde schema ──────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct NumberingXml {
    #[serde(rename = "abstractNum", default)]
    abstract_nums: Vec<AbstractNumXml>,
    #[serde(rename = "num", default)]
    nums: Vec<NumXml>,
    /// Picture bullets are parsed structurally here (for id resolution)
    /// but their `<w:pict>` contents are filled in by the pre-pass.
    #[serde(rename = "numPicBullet", default)]
    num_pic_bullets: Vec<NumPicBulletXml>,
}

#[derive(Deserialize)]
struct AbstractNumXml {
    #[serde(rename = "@abstractNumId")]
    abstract_num_id: i64,
    #[serde(rename = "lvl", default)]
    levels: Vec<LvlXml>,
}

#[derive(Deserialize)]
struct LvlXml {
    #[serde(rename = "@ilvl")]
    ilvl: u8,
    #[serde(rename = "numFmt", default)]
    num_fmt: Option<ValAttr<StNumberFormat>>,
    #[serde(rename = "lvlText", default)]
    lvl_text: Option<ValString>,
    #[serde(rename = "start", default)]
    start: Option<ValAttr<u32>>,
    #[serde(rename = "lvlJc", default)]
    lvl_jc: Option<ValAttr<StJc>>,
    #[serde(rename = "pPr", default)]
    p_pr: Option<PPrXml>,
    #[serde(rename = "rPr", default)]
    r_pr: Option<RPrXml>,
    #[serde(rename = "lvlPicBulletId", default)]
    lvl_pic_bullet_id: Option<ValAttr<i64>>,
}

#[derive(Deserialize)]
struct NumXml {
    #[serde(rename = "@numId")]
    num_id: i64,
    #[serde(rename = "abstractNumId", default)]
    abstract_num_id: Option<ValAttr<i64>>,
    #[serde(rename = "lvlOverride", default)]
    overrides: Vec<LvlOverrideXml>,
}

#[derive(Deserialize)]
struct LvlOverrideXml {
    #[serde(rename = "@ilvl")]
    ilvl: u8,
    #[serde(rename = "lvl", default)]
    lvl: Option<LvlXml>,
}

#[derive(Deserialize)]
struct NumPicBulletXml {
    #[serde(rename = "@numPicBulletId")]
    num_pic_bullet_id: i64,
}

#[derive(Deserialize)]
struct ValString {
    #[serde(rename = "@val")]
    val: String,
}

#[derive(Deserialize)]
#[serde(bound(deserialize = "T: serde::Deserialize<'de>"))]
struct ValAttr<T> {
    #[serde(rename = "@val")]
    val: T,
}

// ── schema → model ────────────────────────────────────────────────────────

impl From<NumberingXml> for NumberingDefinitions {
    fn from(x: NumberingXml) -> Self {
        let mut defs = NumberingDefinitions::default();
        for a in x.abstract_nums {
            let id = AbstractNumId::new(a.abstract_num_id);
            defs.abstract_nums
                .insert(id, AbstractNumbering { levels: a.levels.into_iter().map(Into::into).collect() });
        }
        for n in x.nums {
            defs.numbering_instances.insert(NumId::new(n.num_id), convert_num(n));
        }
        for bullet in x.num_pic_bullets {
            let id = NumPicBulletId::new(bullet.num_pic_bullet_id);
            // pict filled in by pre-pass
            defs.pic_bullets
                .insert(id, NumPicBullet { id, pict: None });
        }
        defs
    }
}

impl From<LvlXml> for NumberingLevelDefinition {
    fn from(x: LvlXml) -> Self {
        let (indentation, run_properties) = extract_level_properties(x.p_pr, x.r_pr);
        Self {
            level: x.ilvl,
            format: x.num_fmt.map(|v| NumberFormat::from(v.val)),
            level_text: x.lvl_text.map(|v| v.val).unwrap_or_default(),
            start: x.start.map(|v| v.val),
            justification: x.lvl_jc.map(|v| Alignment::from(v.val)),
            indentation,
            run_properties,
            lvl_pic_bullet_id: x.lvl_pic_bullet_id.map(|v| NumPicBulletId::new(v.val)),
        }
    }
}

fn extract_level_properties(
    p_pr: Option<PPrXml>,
    r_pr: Option<RPrXml>,
) -> (Option<Indentation>, Option<RunProperties>) {
    let indentation = p_pr.and_then(|p| p.split().properties.indentation);
    let run_properties = r_pr.map(|r| r.split().0);
    (indentation, run_properties)
}

fn convert_num(n: NumXml) -> NumberingInstance {
    let abstract_num_id = n
        .abstract_num_id
        .map(|v| AbstractNumId::new(v.val))
        .unwrap_or_else(|| AbstractNumId::new(0));
    let level_overrides = n
        .overrides
        .into_iter()
        .filter_map(|o| {
            o.lvl.map(|mut lvl| {
                lvl.ilvl = o.ilvl; // legacy parser used override's @ilvl, not inner lvl's
                NumberingLevelDefinition::from(lvl)
            })
        })
        .collect();
    NumberingInstance {
        abstract_num_id,
        level_overrides,
    }
}
