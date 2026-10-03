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
    /// SHA-256 of that executable, lower case hexadecimal. The primary key.
    pub sha256: String,
    /// Size of the executable in bytes. Cheap to check before hashing.
    pub file_size: u64,
    /// PE timestamp, for humans comparing builds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pe_timestamp: Option<String>,
    /// Size of the mapped image, for humans.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_size: Option<String>,
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
      "offsets": { "framebase.rotation": { "value": "0x08" } }
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
