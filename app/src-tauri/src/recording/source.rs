//! Shared capture descriptions. Native window identities never cross public IPC.
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Region {
    pub display_id: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Source {
    Region(Region),
    Window {
        #[serde(rename = "windowId")]
        window_id: String,
    },
}
impl<'de> Deserialize<'de> for Source {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
        enum Tagged {
            Region(Region),
            Window {
                #[serde(rename = "windowId")]
                window_id: String,
            },
        }
        let value = serde_json::Value::deserialize(deserializer)?;
        // Old recovery journals contain a plain Region. An explicit bad kind
        // must never be interpreted as a legacy monitor capture.
        if value.get("kind").is_none() {
            return serde_json::from_value(value)
                .map(Source::Region)
                .map_err(serde::de::Error::custom);
        }
        match serde_json::from_value(value).map_err(serde::de::Error::custom)? {
            Tagged::Region(region) => Ok(Source::Region(region)),
            Tagged::Window { window_id } => Ok(Source::Window { window_id }),
        }
    }
}
impl Source {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Region(region)
                if !region.display_id.is_empty() && region.display_id.len() <= 256 =>
            {
                Geometry {
                    width: region.width,
                    height: region.height,
                }
                .validate()
            }
            Self::Window { window_id }
                if !window_id.is_empty()
                    && window_id.len() <= 100
                    && window_id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-') =>
            {
                Ok(())
            }
            _ => Err("Choose an available recording source.".into()),
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
}
impl Geometry {
    pub fn validate(self) -> Result<(), String> {
        if self.width < 16 || self.height < 16 || self.width > 7680 || self.height > 4320 {
            Err("Choose a source between 16 pixels and 7680 × 4320.".into())
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)] // Constructed only by the isolated native helper.
pub struct Display {
    pub id: String,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WindowIdentity {
    pub hwnd: String,
    pub process_id: u32,
    pub process_started: String,
    pub class_name: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    pub identity: WindowIdentity,
    pub name: String,
    pub process_name: String,
    pub width: u32,
    pub height: u32,
    pub display_id: String,
    pub available: bool,
    pub reason: Option<String>,
    pub protection: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowListing {
    pub windows: Vec<WindowInfo>,
    pub truncated: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum NativeSource {
    Region(Region),
    Window { identity: WindowIdentity },
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_region_journals_are_read_but_bad_explicit_kinds_never_fall_back() {
        let region =
            serde_json::json!({"displayId":"display", "x":0,"y":0,"width":641,"height":481});
        let parsed: Source = serde_json::from_value(region.clone()).unwrap();
        assert!(matches!(parsed, Source::Region(_)));
        let mut bad = region;
        bad["kind"] = serde_json::json!("window");
        assert!(serde_json::from_value::<Source>(bad.clone()).is_err());
        bad["kind"] = serde_json::json!("other");
        assert!(serde_json::from_value::<Source>(bad).is_err());
        let encoded = serde_json::to_value(parsed).unwrap();
        assert_eq!(encoded["kind"], "region");
        assert!(serde_json::from_value::<Source>(encoded).is_ok());
    }
}
