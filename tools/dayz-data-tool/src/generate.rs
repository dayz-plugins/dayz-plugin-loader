//! Turning a list of known addresses into a build file plus candidate patterns.

use std::collections::BTreeMap;

use dayz_data::{
    BuildFile, BuildInfo, EventEntry, OffsetEntry, Pattern, PatternEntry, PatternFile, Provenance,
    SymbolEntry, SymbolKind, SCHEMA_VERSION,
};
use serde::Deserialize;

use crate::image::Image;

/// Bytes of an instruction check. The predecessor project used 16; 12 is enough to be
/// distinctive and short enough to survive a one-instruction change at the end.
const CHECK_LEN: usize = 12;

/// Shortest candidate pattern, then grown while it is not unique.
const PATTERN_MIN: usize = 12;

/// Longest candidate pattern. Past this the signature is almost certainly matching
/// build-specific operands rather than the code's shape.
const PATTERN_MAX: usize = 48;

/// What a person hands the tool: addresses they have established, without the derived parts.
#[derive(Debug, Deserialize)]
pub struct Seed {
    /// Human readable build number.
    pub version: String,
    /// Addresses by symbol name.
    #[serde(default)]
    pub symbols: BTreeMap<String, SeedSymbol>,
    /// Struct field offsets by name.
    #[serde(default)]
    pub offsets: BTreeMap<String, OffsetEntry>,
    /// Event classes by the name the engine reports for them.
    ///
    /// Copied through unchanged. There is nothing to derive: an event has no address to find
    /// a pattern for, and a field offset has no bytes to check, so the seed is already the
    /// finished form. It passes through here only so that one file per build stays the one
    /// place a person edits.
    #[serde(default)]
    pub events: BTreeMap<String, EventEntry>,
}

/// One seeded address.
#[derive(Debug, Deserialize)]
pub struct SeedSymbol {
    /// Address relative to the image base, as `"0x8E77C0"`.
    pub rva: String,
    /// What the symbol names.
    #[serde(default)]
    pub kind: SymbolKind,
    /// What it is for.
    #[serde(default)]
    pub note: Option<String>,
}

/// What generation produced, alongside the files.
#[derive(Debug, Default)]
pub struct Report {
    /// Symbols that got a check and a unique candidate pattern.
    pub patterned: Vec<String>,
    /// Symbols whose bytes are not unique even at the maximum length, so no pattern was
    /// emitted. They still get a cached address and a check.
    pub not_unique: Vec<String>,
    /// Symbols whose address lies outside the image, which means the seed is wrong.
    pub out_of_image: Vec<String>,
}

fn parse_rva(text: &str) -> Option<u64> {
    let body = text
        .trim()
        .strip_prefix("0x")
        .or_else(|| text.trim().strip_prefix("0X"))?;
    u64::from_str_radix(body, 16).ok()
}

/// Bytes at `rva`, as a pattern of literal bytes.
fn literal(image: &[u8], rva: usize, len: usize) -> Option<Pattern> {
    let bytes = image.get(rva..rva + len)?;
    let text: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
    text.join(" ").parse().ok()
}

/// The shortest literal run at `rva` that occurs exactly once in the image.
fn unique_pattern(image: &[u8], rva: usize) -> Option<Pattern> {
    (PATTERN_MIN..=PATTERN_MAX).step_by(4).find_map(|len| {
        let candidate = literal(image, rva, len)?;
        candidate.find_unique(image).map(|_| candidate)
    })
}

/// Build a build file and a pattern file from a seed and the mapped image.
pub fn generate(
    seed: &Seed,
    image: &Image,
    executable: &str,
    sha256: &str,
    today: &str,
) -> (BuildFile, PatternFile, Report) {
    let mut report = Report::default();
    let mut symbols = BTreeMap::new();
    let mut patterns = BTreeMap::new();

    for (name, seeded) in &seed.symbols {
        let Some(rva) = parse_rva(&seeded.rva) else {
            report.out_of_image.push(name.clone());
            continue;
        };
        let Ok(at) = usize::try_from(rva) else {
            report.out_of_image.push(name.clone());
            continue;
        };
        let literal_bytes = literal(&image.bytes, at, CHECK_LEN);
        if literal_bytes.is_none() {
            report.out_of_image.push(name.clone());
            continue;
        }
        // A global's bytes are data the game writes at runtime. In the file they are whatever
        // the image initialises them to, usually zeros, so a check over them would fail on
        // every launch and a signature over them would identify nothing. Only code gets
        // either; the global still gets its address.
        let is_code = seeded.kind != SymbolKind::Global;
        symbols.insert(
            name.clone(),
            SymbolEntry {
                rva,
                kind: seeded.kind,
                check: literal_bytes.filter(|_| is_code),
                note: seeded.note.clone(),
            },
        );
        if !is_code {
            continue;
        }
        match unique_pattern(&image.bytes, at) {
            Some(pattern) => {
                report.patterned.push(name.clone());
                patterns.insert(
                    name.clone(),
                    PatternEntry {
                        patterns: vec![pattern],
                        resolve: dayz_data::Resolve::Direct,
                        note: seeded.note.clone(),
                        since: Some(seed.version.clone()),
                    },
                );
            }
            None => report.not_unique.push(name.clone()),
        }
    }

    let build = BuildFile {
        schema: SCHEMA_VERSION,
        build: BuildInfo {
            version: seed.version.clone(),
            executable: executable.to_owned(),
            sha256: Some(sha256.to_owned()),
            file_size: Some(image.file_size),
            pe_timestamp: Some(u64::from(image.timestamp)),
            image_size: Some(u64::from(image.size_of_image)),
            verified: Some(today.to_owned()),
            provenance: Provenance::Analysis,
        },
        symbols,
        offsets: seed.offsets.clone(),
        events: seed.events.clone(),
    };
    (
        build,
        PatternFile {
            schema: SCHEMA_VERSION,
            symbols: patterns,
        },
        report,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> Image {
        let mut bytes = vec![0x90; 0x400];
        // A distinctive run at 0x100, and the same run repeated at 0x200 so it is not unique.
        let run: Vec<u8> = (0u8..64).collect();
        bytes[0x100..0x140].copy_from_slice(&run);
        bytes[0x200..0x240].copy_from_slice(&run);
        // A run that is unique.
        let other: Vec<u8> = (128u8..192).collect();
        bytes[0x300..0x340].copy_from_slice(&other);
        Image {
            bytes,
            size_of_image: 0x400,
            timestamp: 0x6A72_FC58,
            file_size: 1234,
        }
    }

    fn seed() -> Seed {
        let json = r#"{
          "version": "1.29.163709",
          "symbols": {
            "unique.fn":    { "rva": "0x300", "note": "has a unique run" },
            "repeated.fn":  { "rva": "0x100" },
            "a.global":     { "rva": "0x300", "kind": "global" },
            "outside.fn":   { "rva": "0x9000" },
            "nonsense.fn":  { "rva": "not hex" }
          },
          "offsets": { "framebase.rotation": { "value": "0x08" } }
        }"#;
        serde_json::from_str(json).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn generates_checks_patterns_and_metadata() {
        let (build, patterns, report) =
            generate(&seed(), &image(), "DayZ_x64.exe", "abc", "2026-10-03");

        assert_eq!(build.build.pe_timestamp, Some(0x6A72_FC58));
        assert_eq!(build.build.image_size, Some(0x400));
        assert_eq!(build.build.file_size, Some(1234));
        assert_eq!(build.build.provenance, Provenance::Analysis);
        assert_eq!(build.offsets["framebase.rotation"].value, 8);

        // Every in-image symbol gets an address, and code gets a check even without a
        // pattern. A global gets neither a check nor a pattern: its file bytes are not what
        // memory holds at runtime, so a check over them would fail on every launch.
        assert_eq!(build.symbols.len(), 3);
        assert_eq!(build.symbols["a.global"].check, None);
        assert_eq!(build.symbols["unique.fn"].rva, 0x300);
        assert_eq!(
            build.symbols["repeated.fn"]
                .check
                .as_ref()
                .map(Pattern::len),
            Some(CHECK_LEN)
        );

        // Only the unique code run becomes a pattern; the global and the repeated run do not.
        assert_eq!(report.patterned, ["unique.fn"]);
        assert_eq!(report.not_unique, ["repeated.fn"]);
        assert_eq!(report.out_of_image, ["nonsense.fn", "outside.fn"]);
        assert_eq!(patterns.symbols.keys().collect::<Vec<_>>(), ["unique.fn"]);
        assert_eq!(
            patterns.symbols["unique.fn"].since.as_deref(),
            Some("1.29.163709")
        );
        assert!(
            !patterns.symbols.contains_key("a.global"),
            "a global's bytes are not a signature"
        );
    }

    #[test]
    fn a_generated_pattern_matches_exactly_where_it_came_from() {
        let image = image();
        let (_, patterns, _) = generate(&seed(), &image, "DayZ_x64.exe", "abc", "2026-10-03");
        let pattern = &patterns.symbols["unique.fn"].patterns[0];
        assert_eq!(pattern.find_unique(&image.bytes), Some(0x300));
    }

    #[test]
    fn patterns_grow_only_as_far_as_the_maximum() {
        let mut bytes = vec![0x90; 0x200];
        // 40 identical bytes then one distinctive one, beyond PATTERN_MAX from the start.
        bytes[0x100..0x100 + 60].copy_from_slice(&[0xAB; 60]);
        bytes[0x160] = 0xCD;
        let image = Image {
            bytes,
            size_of_image: 0x200,
            timestamp: 0,
            file_size: 0,
        };
        let seed: Seed =
            serde_json::from_str(r#"{"version":"t","symbols":{"flat.fn":{"rva":"0x100"}}}"#)
                .unwrap_or_else(|e| panic!("{e}"));
        let (build, patterns, report) = generate(&seed, &image, "e", "s", "d");
        assert!(patterns.symbols.is_empty());
        assert_eq!(report.not_unique, ["flat.fn"]);
        assert!(
            build.symbols.contains_key("flat.fn"),
            "the address is still cached"
        );
    }
}
