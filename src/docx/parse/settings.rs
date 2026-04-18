//! Parser for `word/settings.xml`.

use serde::{Deserialize, Deserializer};

use crate::docx::dimension::Dimension;
use crate::docx::error::Result;
use crate::docx::model::{DocumentSettings, RevisionSaveId};
use crate::docx::parse::serde_xml::from_xml;

/// Parse `word/settings.xml`. Entry point: deserializes into an intermediate
/// schema, then maps to the model type.
pub fn parse_settings(data: &[u8]) -> Result<DocumentSettings> {
    from_xml::<SettingsXml>(data).map(Into::into)
}

#[derive(Deserialize, Default)]
struct SettingsXml {
    #[serde(rename = "defaultTabStop", default)]
    default_tab_stop: Option<ValI64>,
    #[serde(rename = "evenAndOddHeaders", default)]
    even_and_odd_headers: Option<OnOff>,
    #[serde(default)]
    rsids: Option<RsidsXml>,
}

#[derive(Deserialize, Default)]
struct RsidsXml {
    #[serde(rename = "rsidRoot", default)]
    rsid_root: Option<ValString>,
    #[serde(rename = "rsid", default)]
    rsids: Vec<ValString>,
}

#[derive(Deserialize)]
struct ValI64 {
    #[serde(rename = "@val")]
    val: i64,
}

#[derive(Deserialize)]
struct ValString {
    #[serde(rename = "@val")]
    val: String,
}

/// OOXML ST_OnOff toggle element. An absent `@val` means the toggle is on
/// (§17.18.68), matching how `<w:b/>`, `<w:evenAndOddHeaders/>` etc. behave.
struct OnOff(bool);

impl<'de> Deserialize<'de> for OnOff {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(rename = "@val", default)]
            val: Option<String>,
        }
        let raw = Raw::deserialize(d)?;
        let on = match raw.val.as_deref() {
            None => true,
            Some("true" | "1" | "on") => true,
            Some("false" | "0" | "off") => false,
            Some(_) => true,
        };
        Ok(OnOff(on))
    }
}

impl From<SettingsXml> for DocumentSettings {
    fn from(x: SettingsXml) -> Self {
        let mut s = DocumentSettings::default();
        if let Some(t) = x.default_tab_stop {
            s.default_tab_stop = Dimension::new(t.val);
        }
        if let Some(OnOff(on)) = x.even_and_odd_headers {
            s.even_and_odd_headers = on;
        }
        if let Some(r) = x.rsids {
            if let Some(root) = r.rsid_root {
                s.rsid_root = RevisionSaveId::from_hex(&root.val);
            }
            s.rsids = r
                .rsids
                .into_iter()
                .filter_map(|v| RevisionSaveId::from_hex(&v.val))
                .collect();
        }
        s
    }
}
