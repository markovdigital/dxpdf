//! `Deserialize` for `Dimension<U>`. Makes OOXML numeric attributes with a
//! unit marker (twips, EMU, half-points, etc.) usable directly in schema
//! structs without hand-written wrapper types.

use serde::{Deserialize, Deserializer};

use super::integer_measure::IntegerMeasure;
use crate::model::dimension::{Dimension, Unit};

pub(crate) fn deserialize_nonnegative_dimension<'de, D, U>(
    deserializer: D,
) -> Result<Dimension<U>, D::Error>
where
    D: Deserializer<'de>,
    U: Unit,
{
    let measure = IntegerMeasure::deserialize(deserializer)?;
    if measure.is_negative() {
        return Err(serde::de::Error::custom(
            "negative value is not valid for this OOXML measurement",
        ));
    }
    Ok(Dimension::new(measure.value()))
}

pub(crate) fn deserialize_optional_nonnegative_dimension<'de, D, U>(
    deserializer: D,
) -> Result<Option<Dimension<U>>, D::Error>
where
    D: Deserializer<'de>,
    U: Unit,
{
    Option::<IntegerMeasure>::deserialize(deserializer)?.map_or(Ok(None), |measure| {
        if measure.is_negative() {
            Err(serde::de::Error::custom(
                "negative value is not valid for this OOXML measurement",
            ))
        } else {
            Ok(Some(Dimension::new(measure.value())))
        }
    })
}

impl<'de, U: Unit> Deserialize<'de> for Dimension<U> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Dimension::new(IntegerMeasure::deserialize(d)?.value()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::dimension::{Emu, HalfPoints, Twips};
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct TwipsVal {
        #[serde(rename = "@val")]
        val: Dimension<Twips>,
    }

    #[derive(Deserialize)]
    struct Sample {
        #[serde(rename = "@w")]
        w: Dimension<Emu>,
        #[serde(rename = "@h")]
        h: Dimension<HalfPoints>,
    }

    #[test]
    fn twips_attribute_deserializes() {
        let v: TwipsVal = quick_xml::de::from_str(r#"<x val="720"/>"#).unwrap();
        assert_eq!(v.val.raw(), 720);
    }

    #[test]
    fn mixed_unit_attributes() {
        let s: Sample = quick_xml::de::from_str(r#"<ext w="914400" h="400"/>"#).unwrap();
        assert_eq!(s.w.raw(), 914_400);
        assert_eq!(s.h.raw(), 400);
    }

    #[test]
    fn negative_values_preserved() {
        let v: TwipsVal = quick_xml::de::from_str(r#"<x val="-120"/>"#).unwrap();
        assert_eq!(v.val.raw(), -120);
    }

    #[test]
    fn non_integer_rejected() {
        let r: Result<TwipsVal, _> = quick_xml::de::from_str(r#"<x val="abc"/>"#);
        assert!(
            r.is_err(),
            "expected error, got {:?}",
            r.map(|v| v.val.raw())
        );
    }
}
