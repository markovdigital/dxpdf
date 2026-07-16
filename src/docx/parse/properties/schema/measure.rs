//! Table measure (§17.18.87 ST_TblWidth) — a discriminated width value used
//! by `<w:tblW>`, `<w:tcW>`, `<w:tblInd>`, `<w:tblCellSpacing>`, `<w:wAfter>`.
//!
//! `@type` picks the interpretation of `@w`:
//! - `dxa` → twips
//! - `pct` → percentage (stored as thousandths-of-percent)
//! - `auto` / `nil` → no explicit value

use serde::Deserialize;

use crate::docx::model::dimension::{Dimension, ThousandthPercent};
use crate::docx::model::TableMeasure;
use crate::docx::parse::primitives::integer_measure::IntegerMeasure;

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum StTblWidthType {
    Auto,
    Dxa,
    Nil,
    Pct,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub(crate) struct TableMeasureXml {
    #[serde(rename = "@w", default)]
    w: Option<IntegerMeasure>,
    #[serde(rename = "@type", default = "default_type")]
    ty: StTblWidthType,
}

fn default_type() -> StTblWidthType {
    StTblWidthType::Auto
}

impl From<TableMeasureXml> for TableMeasure {
    fn from(x: TableMeasureXml) -> Self {
        let value = x.w.map(IntegerMeasure::value).unwrap_or(0);
        match x.ty {
            StTblWidthType::Auto => Self::Auto,
            StTblWidthType::Nil => Self::Nil,
            StTblWidthType::Dxa => Self::Twips(Dimension::new(value)),
            StTblWidthType::Pct => Self::Pct(Dimension::<ThousandthPercent>::new(value)),
        }
    }
}

pub(crate) fn deserialize_optional_nonnegative_table_measure<'de, D>(
    deserializer: D,
) -> Result<Option<TableMeasureXml>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let measure = Option::<TableMeasureXml>::deserialize(deserializer)?;
    if measure
        .as_ref()
        .and_then(|value| value.w.as_ref())
        .is_some_and(IntegerMeasure::is_negative)
    {
        return Err(serde::de::Error::custom(
            "negative value is not valid for this OOXML table measurement",
        ));
    }
    Ok(measure)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(xml: &str) -> TableMeasure {
        let x: TableMeasureXml = quick_xml::de::from_str(xml).unwrap();
        x.into()
    }

    #[test]
    fn dxa_twips() {
        match parse(r#"<tblW w="5000" type="dxa"/>"#) {
            TableMeasure::Twips(d) => assert_eq!(d.raw(), 5000),
            other => panic!("expected Twips, got {other:?}"),
        }
    }

    #[test]
    fn pct_thousandth_percent() {
        match parse(r#"<tblW w="2500" type="pct"/>"#) {
            TableMeasure::Pct(d) => assert_eq!(d.raw(), 2500),
            other => panic!("expected Pct, got {other:?}"),
        }
    }

    #[test]
    fn auto_and_nil() {
        assert!(matches!(
            parse(r#"<tblW w="0" type="auto"/>"#),
            TableMeasure::Auto
        ));
        assert!(matches!(
            parse(r#"<tblW w="0" type="nil"/>"#),
            TableMeasure::Nil
        ));
    }

    #[test]
    fn missing_type_defaults_to_auto() {
        assert!(matches!(parse(r#"<tblW/>"#), TableMeasure::Auto));
    }

    #[test]
    fn decimal_widths_round_to_nearest_integer() {
        match parse(r#"<tblW w="2500.4" type="dxa"/>"#) {
            TableMeasure::Twips(d) => assert_eq!(d.raw(), 2500),
            other => panic!("expected Twips, got {other:?}"),
        }
        match parse(r#"<tblW w="2500.5" type="dxa"/>"#) {
            TableMeasure::Twips(d) => assert_eq!(d.raw(), 2501),
            other => panic!("expected Twips, got {other:?}"),
        }
    }
}
