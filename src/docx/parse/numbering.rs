//! Parser for `word/numbering.xml` — single-pass serde over the whole file.
//! Picture bullets' `<w:pict>` contents are deserialized via the VML schema.

use serde::Deserialize;

use crate::docx::error::Result;
use crate::docx::model::{
    AbstractNumId, AbstractNumbering, Alignment, Indentation, NumId, NumPicBullet, NumPicBulletId,
    NumberFormat, NumberingDefinitions, NumberingInstance, NumberingLevelDefinition, RunProperties,
};
use crate::docx::parse::primitives::st_enums::{StJc, StNumberFormat};
use crate::docx::parse::properties::schema::paragraph::PPrXml;
use crate::docx::parse::properties::schema::run::RPrXml;
use crate::docx::parse::serde_xml::from_xml;

pub fn parse_numbering(data: &[u8]) -> Result<NumberingDefinitions> {
    if data.is_empty() {
        return Ok(NumberingDefinitions::default());
    }
    let schema: NumberingXml = from_xml(data)?;
    Ok(schema.into())
}

// ── serde schema ──────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct NumberingXml {
    #[serde(rename = "$value", default)]
    children: Vec<NumberingChildXml>,
}

#[derive(Deserialize)]
enum NumberingChildXml {
    #[serde(rename = "abstractNum")]
    AbstractNum(AbstractNumXml),
    #[serde(rename = "num")]
    Num(NumXml),
    #[serde(rename = "numPicBullet")]
    NumPicBullet(Box<NumPicBulletXml>),
    #[serde(other)]
    Unknown,
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
    #[serde(rename = "pict", default)]
    pict: Option<crate::docx::parse::vml::schema::PictXml>,
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
        // Picture bullets may contain a VML `<w:pict>` (e.g., an imagedata
        // reference). Numbering has no body content, so no embeds crossing
        // into body convert — pass an empty ctx.
        let mut ctx = crate::docx::parse::body::ConvertCtx::new();
        for child in x.children {
            match child {
                NumberingChildXml::AbstractNum(a) => {
                    let id = AbstractNumId::new(a.abstract_num_id);
                    defs.abstract_nums.insert(
                        id,
                        AbstractNumbering {
                            levels: a.levels.into_iter().map(Into::into).collect(),
                        },
                    );
                }
                NumberingChildXml::Num(n) => {
                    defs.numbering_instances
                        .insert(NumId::new(n.num_id), convert_num(n));
                }
                NumberingChildXml::NumPicBullet(bullet) => {
                    let id = NumPicBulletId::new(bullet.num_pic_bullet_id);
                    let pict = bullet.pict.map(|p| p.into_model(&mut ctx));
                    defs.pic_bullets.insert(id, NumPicBullet { id, pict });
                }
                NumberingChildXml::Unknown => {}
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbering_ids_remain_strict_integers() {
        let xml = br#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="1.0"/></w:numbering>"#;
        assert!(parse_numbering(xml).is_err());
    }

    #[test]
    fn repeated_abstract_and_concrete_numbering_definitions_are_collected() {
        let xml = br#"
          <w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:lvlText w:val="A%1"/></w:lvl></w:abstractNum>
            <w:num w:numId="11"><w:abstractNumId w:val="1"/></w:num>
            <w:abstractNum w:abstractNumId="2"><w:lvl w:ilvl="0"><w:lvlText w:val="B%1"/></w:lvl></w:abstractNum>
            <w:num w:numId="12"><w:abstractNumId w:val="2"/></w:num>
          </w:numbering>"#;
        let defs = parse_numbering(xml).unwrap();
        assert_eq!(defs.abstract_nums.len(), 2);
        assert_eq!(defs.numbering_instances.len(), 2);
        assert_eq!(
            defs.numbering_instances[&NumId::new(11)].abstract_num_id,
            AbstractNumId::new(1)
        );
        assert_eq!(
            defs.numbering_instances[&NumId::new(12)].abstract_num_id,
            AbstractNumId::new(2)
        );
    }

    #[test]
    fn unknown_numbering_root_children_do_not_discard_known_definitions() {
        let xml = br#"
          <w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:abstractNum w:abstractNumId="3"/>
            <w:futureExtension><w:nested w:val="ignored"/></w:futureExtension>
            <w:num w:numId="13"><w:abstractNumId w:val="3"/></w:num>
          </w:numbering>"#;
        let defs = parse_numbering(xml).unwrap();
        assert!(defs.abstract_nums.contains_key(&AbstractNumId::new(3)));
        assert!(defs.numbering_instances.contains_key(&NumId::new(13)));
    }
}
