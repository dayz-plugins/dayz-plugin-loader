//! Key metadata for the editor: display names, descriptions, types, ranges, enum values
//! and live/restart flags. Read from `dayz_openxr.schema.json`; the copy in the
//! repository is compiled into the binary, a file of that name beside the ini wins.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

/// The repository schema, embedded so the binary needs no files beside it.
const EMBEDDED: &str = include_str!("../../../dayz_openxr.schema.json");

/// How a key should be edited, as declared by the schema.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyType {
    Bool,
    Int {
        min: Option<i64>,
        max: Option<i64>,
        step: i64,
    },
    Float {
        min: Option<f64>,
        max: Option<f64>,
        step: f64,
    },
    Enum(Vec<String>),
    Text,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeyInfo {
    pub title: String,
    pub description: String,
    pub kind: KeyType,
    /// Changeable in the running game through the debug plugin.
    pub live: bool,
    /// Read only when the hooks install; the game must be restarted.
    pub restart: bool,
    /// Present in the file but not read by the current build.
    pub unused: bool,
    /// Unit shown next to the value (seconds, metres, degrees, ...).
    pub unit: String,
    /// The value the project ships with, as ini text; the per-key reset target.
    pub default: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SectionInfo {
    pub title: String,
    pub description: String,
    pub keys: BTreeMap<String, KeyInfo>,
}

/// The whole schema: section name → section info.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Schema {
    sections: BTreeMap<String, SectionInfo>,
}

impl Schema {
    /// The schema compiled into the binary.
    #[must_use]
    pub fn embedded() -> Self {
        Self::from_json(EMBEDDED).unwrap_or_default()
    }

    /// `dayz_openxr.schema.json` beside `ini_path` when present, else the embedded copy.
    #[must_use]
    pub fn for_ini(ini_path: &Path) -> Self {
        let beside = ini_path.with_file_name("dayz_openxr.schema.json");
        std::fs::read_to_string(beside)
            .ok()
            .and_then(|text| Self::from_json(&text).ok())
            .unwrap_or_else(Self::embedded)
    }

    /// Parses schema JSON.
    ///
    /// # Errors
    /// Returns the JSON error for malformed text; unknown fields are ignored.
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        let root: Value = serde_json::from_str(text)?;
        let mut sections = BTreeMap::new();
        if let Some(map) = root.get("sections").and_then(Value::as_object) {
            for (name, value) in map {
                sections.insert(name.clone(), parse_section(value));
            }
        }
        Ok(Self { sections })
    }

    #[must_use]
    pub fn section(&self, name: &str) -> Option<&SectionInfo> {
        self.sections.get(name)
    }

    #[must_use]
    pub fn key(&self, section: &str, key: &str) -> Option<&KeyInfo> {
        self.sections.get(section)?.keys.get(key)
    }

    /// Every `section.key` the schema describes.
    #[cfg(test)]
    pub fn names(&self) -> impl Iterator<Item = String> + '_ {
        self.sections
            .iter()
            .flat_map(|(section, info)| info.keys.keys().map(move |key| format!("{section}.{key}")))
    }
}

fn text(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn parse_section(value: &Value) -> SectionInfo {
    let mut keys = BTreeMap::new();
    if let Some(map) = value.get("keys").and_then(Value::as_object) {
        for (key, info) in map {
            keys.insert(key.clone(), parse_key(info));
        }
    }
    SectionInfo {
        title: text(value, "title"),
        description: text(value, "description"),
        keys,
    }
}

fn parse_key(value: &Value) -> KeyInfo {
    let kind = match value.get("type").and_then(Value::as_str).unwrap_or("text") {
        "bool" => KeyType::Bool,
        "int" => KeyType::Int {
            min: value.get("min").and_then(Value::as_i64),
            max: value.get("max").and_then(Value::as_i64),
            step: value.get("step").and_then(Value::as_i64).unwrap_or(1),
        },
        "float" => KeyType::Float {
            min: value.get("min").and_then(Value::as_f64),
            max: value.get("max").and_then(Value::as_f64),
            step: value.get("step").and_then(Value::as_f64).unwrap_or(0.1),
        },
        "enum" => KeyType::Enum(
            value
                .get("values")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
        ),
        _ => KeyType::Text,
    };
    KeyInfo {
        title: text(value, "title"),
        description: text(value, "description"),
        kind,
        live: value.get("live").and_then(Value::as_bool).unwrap_or(false),
        restart: value
            .get("restart")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        unused: value
            .get("unused")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        unit: text(value, "unit"),
        default: value.get("default").map(|default| match default {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_schema_parses_and_covers_the_repository_ini() {
        let schema = Schema::embedded();
        assert!(
            schema.section("stereo").is_some(),
            "embedded schema is empty"
        );
        let ini = include_str!("../../../dayz_openxr.ini");
        let doc = crate::ini::Document::parse(ini);
        let mut missing = Vec::new();
        for section in doc.sections() {
            for entry in section.entries {
                if schema.key(&entry.section, &entry.key).is_none() {
                    missing.push(entry.tunable_name());
                }
            }
        }
        assert!(
            missing.is_empty(),
            "ini keys without schema entry: {missing:?}"
        );
        let names: Vec<String> = schema.names().collect();
        let extra: Vec<&String> = names
            .iter()
            .filter(|name| {
                let (section, key) = name.split_once('.').unwrap_or(("", ""));
                doc.get(section, key).is_none()
            })
            .collect();
        assert!(
            extra.is_empty(),
            "schema entries without ini key: {extra:?}"
        );
    }

    #[test]
    fn key_types_and_defaults() {
        let schema = Schema::from_json(
            r#"{"sections":{"s":{"title":"S","keys":{
                "a":{"type":"float","min":0,"max":1},
                "b":{"type":"enum","values":["x","y"],"live":true},
                "c":{"type":"int","step":5,"restart":true,"default":"7"},
                "d":{}}}}}"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let a = schema.key("s", "a").unwrap_or_else(|| panic!("a"));
        assert_eq!(
            a.kind,
            KeyType::Float {
                min: Some(0.0),
                max: Some(1.0),
                step: 0.1
            }
        );
        let b = schema.key("s", "b").unwrap_or_else(|| panic!("b"));
        assert_eq!(b.kind, KeyType::Enum(vec!["x".into(), "y".into()]));
        assert!(b.live && !b.restart);
        let c = schema.key("s", "c").unwrap_or_else(|| panic!("c"));
        assert_eq!(
            c.kind,
            KeyType::Int {
                min: None,
                max: None,
                step: 5
            }
        );
        assert!(c.restart);
        assert_eq!(c.default.as_deref(), Some("7"));
        assert_eq!(a.default, None);
        assert_eq!(
            schema.key("s", "d").map(|k| k.kind.clone()),
            Some(KeyType::Text)
        );
        assert_eq!(schema.key("s", "zzz"), None);
        assert_eq!(schema.section("s").map(|s| s.title.as_str()), Some("S"));
    }
}
