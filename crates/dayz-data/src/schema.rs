//! The JSON shapes stored in a `dayz-data` directory.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::pattern::Pattern;
use crate::resolve::Resolve;

/// Where an entry's addresses came from, which decides how much to trust them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Provenance {
    /// A person established the address and checked it.
    Verified,
    /// Read out of a disassembler, not yet exercised at runtime.
    Analysis,
    /// Produced by the loader scanning patterns against an unknown build. Usable, but it is
    /// a guess until someone confirms it, and the loader says so in the log.
    #[default]
    Scan,
    /// Taken from another project's table. Nobody here has run it, and without the
    /// executable there are no byte checks to catch a wrong entry, so the loader names the
    /// provenance in the log when it uses one.
    External,
}

/// What kind of thing a symbol names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SymbolKind {
    /// Code: a function entry point.
    #[default]
    Function,
    /// Data: a global variable.
    Global,
    /// A specific instruction, for example a call site that gets hooked or patched.
    Instruction,
}

/// Identification of one build of the game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildInfo {
    /// Human readable build number, for example `1.29.163709`.
    pub version: String,
    /// Executable the hash belongs to.
    pub executable: String,
    /// SHA-256 of that executable, lower case hexadecimal. The primary key, and absent only
    /// for an entry contributed without the executable to hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Size of the executable in bytes, when known. Cheap to check before hashing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_size: Option<u64>,
    /// PE timestamp. The secondary key: together with [`BuildInfo::image_size`] it identifies
    /// a build well enough that both predecessor projects gated their hooks on the pair.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::hex::option"
    )]
    pub pe_timestamp: Option<u64>,
    /// Size of the mapped image, the other half of the secondary key.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::hex::option"
    )]
    pub image_size: Option<u64>,
    /// Date the entry was last confirmed, `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified: Option<String>,
    /// How much to trust the addresses.
    #[serde(default)]
    pub provenance: Provenance,
}

/// One resolved address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolEntry {
    /// Address relative to the image base.
    #[serde(with = "crate::hex")]
    pub rva: u64,
    /// What the symbol names.
    #[serde(default)]
    pub kind: SymbolKind,
    /// Bytes expected at `rva`. An entry whose check fails is discarded rather than used,
    /// which is what keeps a stale file from aiming a hook at the wrong instruction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<Pattern>,
    /// What the symbol is for, in a sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One struct field offset. Offsets are signed because a few are expressed backwards from a
/// known field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OffsetEntry {
    /// The offset in bytes.
    #[serde(with = "crate::hex")]
    pub value: u64,
    /// What the field is, in a sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// How to read one field out of an event object, and how to render what is there.
///
/// This is the engine's storage type, not script's: a script `bool` is a four-byte int in the
/// object, and script's `vector` is three consecutive floats. Anything that is a pointer to
/// something with its own class — `PlayerIdentity`, `Man`, `Serializer` — is [`FieldKind::Object`],
/// because following one means knowing that class's layout too, which is a separate question
/// from this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FieldKind {
    /// Four-byte signed integer.
    #[default]
    Int,
    /// Four-byte float.
    Float,
    /// Four-byte integer holding 0 or 1.
    Bool,
    /// Pointer to an engine string holder, or null for the empty string.
    String,
    /// Three consecutive floats.
    Vector,
    /// Pointer to some other object. Reported as the address; not followed.
    Object,
}

/// One field of one event class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventField {
    /// The field's name, taken from the script `Param` the engine builds out of it.
    pub name: String,
    /// Where it sits in the event object, from the object's own address.
    #[serde(with = "crate::hex")]
    pub offset: u64,
    /// How to read it.
    pub kind: FieldKind,
    /// The script-side type, when it says more than [`EventField::kind`] does —
    /// `PlayerIdentity` rather than just "a pointer".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_type: Option<String>,
    /// What the field means, in a sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One event class the engine can raise.
///
/// Keyed by the class name the engine itself reports — the literal its own name getter
/// returns — so the loader can look an event up with nothing but the name it already read out
/// of the vtable. That also makes the entry self-checking in the one way that matters: an
/// entry whose key no longer matches any name the engine produces is simply never used, and
/// cannot send a field read to the wrong offset.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EventEntry {
    /// RVA of the class's vtable, for cross-referencing against a disassembler. The loader
    /// does not need it: it finds events by hooking the one broadcaster, not by address.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::hex::option"
    )]
    pub vtable: Option<u64>,
    /// The fields, in the order the script `Param` lists them. Empty means the class is known
    /// but its contents are not decoded, which is the honest state for most of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<EventField>,
    /// What the event means, in a sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Addresses and offsets for one build: the cache half of the database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildFile {
    /// Schema version; a file from a newer schema is refused rather than guessed at.
    pub schema: u32,
    /// Which build this is.
    pub build: BuildInfo,
    /// Addresses by symbol name.
    #[serde(default)]
    pub symbols: BTreeMap<String, SymbolEntry>,
    /// Struct field offsets by name.
    #[serde(default)]
    pub offsets: BTreeMap<String, OffsetEntry>,
    /// Event classes by the name the engine reports for them.
    #[serde(default)]
    pub events: BTreeMap<String, EventEntry>,
}

/// Signatures for one symbol: the source-of-truth half of the database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternEntry {
    /// Candidate signatures, tried in order. More than one entry is how a signature that
    /// changed shape in a newer build keeps working for the older ones.
    pub patterns: Vec<Pattern>,
    /// What to do with a hit.
    #[serde(default)]
    pub resolve: Resolve,
    /// What the symbol is, in a sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Earliest game build the signature is known to work on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
}

/// Build-independent signatures.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PatternFile {
    /// Schema version.
    pub schema: u32,
    /// Signatures by symbol name.
    #[serde(default)]
    pub symbols: BTreeMap<String, PatternEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILD_JSON: &str = r#"{
      "schema": 1,
      "build": {
        "version": "1.29.163709",
        "executable": "DayZ_x64.exe",
        "sha256": "6e17",
        "file_size": 17851448,
        "pe_timestamp": "0x6A72FC58",
        "provenance": "verified"
      },
      "symbols": {
        "render.frame": { "rva": "0x8E77C0", "check": "48 8B C4", "note": "Per-frame work." },
        "camera.manager": { "rva": "0x1007CE0", "kind": "global" }
      },
      "offsets": { "framebase.rotation": { "value": "0x08" } },
      "events": {
        "ChatMessageEvent": {
          "vtable": "0xCE75B0",
          "fields": [
            { "name": "from", "offset": "0x08", "kind": "string" },
            { "name": "channel", "offset": "0x38", "kind": "int" }
          ]
        },
        "ProgressEvent": {}
      }
    }"#;

    #[test]
    fn reads_a_build_file() {
        let file: BuildFile = serde_json::from_str(BUILD_JSON).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(file.build.provenance, Provenance::Verified);
        let frame = &file.symbols["render.frame"];
        assert_eq!(frame.rva, 0x008E_77C0);
        assert_eq!(
            frame.kind,
            SymbolKind::Function,
            "function is the default kind"
        );
        assert_eq!(
            frame.check.as_ref().map(ToString::to_string).as_deref(),
            Some("48 8B C4")
        );
        assert_eq!(file.symbols["camera.manager"].kind, SymbolKind::Global);
        assert_eq!(file.offsets["framebase.rotation"].value, 8);
    }

    #[test]
    fn reads_event_classes_including_undecoded_ones() {
        let file: BuildFile = serde_json::from_str(BUILD_JSON).unwrap_or_else(|e| panic!("{e}"));
        let chat = &file.events["ChatMessageEvent"];
        assert_eq!(chat.vtable, Some(0x00CE_75B0));
        assert_eq!(chat.fields.len(), 2);
        assert_eq!(chat.fields[0].name, "from");
        assert_eq!(chat.fields[0].offset, 8);
        assert_eq!(chat.fields[0].kind, FieldKind::String);
        assert_eq!(chat.fields[1].kind, FieldKind::Int);
        let progress = &file.events["ProgressEvent"];
        assert_eq!(
            progress,
            &EventEntry::default(),
            "a class with nothing known about it is still a class the loader can name"
        );
    }

    #[test]
    fn a_build_file_round_trips() {
        let file: BuildFile = serde_json::from_str(BUILD_JSON).unwrap_or_else(|e| panic!("{e}"));
        let text = serde_json::to_string(&file).unwrap_or_else(|e| panic!("{e}"));
        let again: BuildFile = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(file, again);
    }

    #[test]
    fn patterns_default_to_direct_resolution_and_scan_provenance() {
        let json = r#"{ "schema": 1, "symbols": {
            "a": { "patterns": ["48 8B C4"] },
            "b": { "patterns": ["48 8B 0D ?? ?? ?? ?? 48"], "resolve": { "kind": "rip_relative", "offset": 3 } }
        } }"#;
        let file: PatternFile = serde_json::from_str(json).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(file.symbols["a"].resolve, Resolve::Direct);
        assert_eq!(
            file.symbols["b"].resolve,
            Resolve::RipRelative {
                offset: 3,
                length: None
            }
        );
        assert_eq!(Provenance::default(), Provenance::Scan);
    }

    #[test]
    fn missing_tables_are_empty_not_an_error() {
        let file: BuildFile = serde_json::from_str(
            r#"{"schema":1,"build":{"version":"x","executable":"y","sha256":"z","file_size":1}}"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(file.symbols.is_empty());
        assert!(file.offsets.is_empty());
        assert_eq!(file.build.provenance, Provenance::Scan);
    }
}
