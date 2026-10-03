//! Resolution: from a database plus a mapped image to a name-to-address table.

use std::collections::BTreeMap;

use crate::pattern::Pattern;
use crate::schema::{BuildFile, PatternFile, SymbolEntry, SymbolKind};

/// Where a resolved address came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Taken from the build file for this executable's hash.
    Cached,
    /// Found by scanning a pattern, because the build file had no entry for it.
    Scanned,
}

/// One resolved symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    /// Address relative to the image base.
    pub rva: u64,
    /// What the symbol names.
    pub kind: SymbolKind,
    /// How it was resolved.
    pub origin: Origin,
}

/// A symbol that could not be resolved, or was resolved and then rejected. Each variant is
/// something a person needs to act on, so they are kept and reported rather than counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Issue {
    /// The cached address does not hold the bytes the entry expects, so it was discarded.
    CheckFailed {
        /// Symbol name.
        name: String,
        /// The address that was checked.
        rva: u64,
    },
    /// The cached address lies outside the image.
    OutOfImage {
        /// Symbol name.
        name: String,
        /// The address that was out of range.
        rva: u64,
    },
    /// No pattern matched anywhere.
    NoMatch {
        /// Symbol name.
        name: String,
    },
    /// A pattern matched in more than one place, so it identifies nothing.
    Ambiguous {
        /// Symbol name.
        name: String,
    },
    /// A pattern matched but its resolution rule failed.
    BadResolve {
        /// Symbol name.
        name: String,
        /// What went wrong.
        reason: String,
    },
    /// Neither the build file nor the pattern file mentions the symbol at all.
    Unknown {
        /// Symbol name.
        name: String,
    },
}

impl Issue {
    /// The symbol the issue concerns.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Issue::CheckFailed { name, .. }
            | Issue::OutOfImage { name, .. }
            | Issue::NoMatch { name }
            | Issue::Ambiguous { name }
            | Issue::BadResolve { name, .. }
            | Issue::Unknown { name } => name,
        }
    }
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Issue::CheckFailed { name, rva } => {
                write!(
                    f,
                    "{name}: the bytes at {rva:#X} are not the ones the database expects"
                )
            }
            Issue::OutOfImage { name, rva } => write!(f, "{name}: {rva:#X} is outside the image"),
            Issue::NoMatch { name } => write!(f, "{name}: no pattern matched"),
            Issue::Ambiguous { name } => write!(f, "{name}: the pattern matched more than once"),
            Issue::BadResolve { name, reason } => write!(f, "{name}: {reason}"),
            Issue::Unknown { name } => write!(f, "{name}: not in the database"),
        }
    }
}

/// Resolved symbols and offsets for the running game.
#[derive(Debug, Clone, Default)]
pub struct SymbolTable {
    symbols: BTreeMap<String, Resolved>,
    offsets: BTreeMap<String, u64>,
    issues: Vec<Issue>,
}

/// Whether the bytes at `rva` match the entry's check. An entry without a check passes:
/// absence of a check is not evidence of a problem.
fn check_passes(image: &[u8], rva: u64, check: Option<&Pattern>) -> bool {
    let Ok(at) = usize::try_from(rva) else {
        return false;
    };
    check.is_none_or(|pattern| pattern.matches_at(image, at))
}

impl SymbolTable {
    /// Resolve every symbol the database knows about against `image`.
    ///
    /// The cache is preferred and verified; anything it does not cover, or whose check
    /// fails, falls back to scanning the patterns.
    #[must_use]
    pub fn resolve(image: &[u8], build: Option<&BuildFile>, patterns: &PatternFile) -> Self {
        let mut table = SymbolTable::default();
        if let Some(build) = build {
            for (name, entry) in &build.symbols {
                table.take_cached(image, name, entry);
            }
            for (name, entry) in &build.offsets {
                table.offsets.insert(name.clone(), entry.value);
            }
        }
        for (name, entry) in &patterns.symbols {
            if table.symbols.contains_key(name) {
                continue;
            }
            table.scan(image, name, entry);
        }
        table
    }

    /// Verify and accept one cached entry.
    fn take_cached(&mut self, image: &[u8], name: &str, entry: &SymbolEntry) {
        if entry.rva >= image.len() as u64 {
            self.issues.push(Issue::OutOfImage {
                name: name.to_owned(),
                rva: entry.rva,
            });
            return;
        }
        if !check_passes(image, entry.rva, entry.check.as_ref()) {
            self.issues.push(Issue::CheckFailed {
                name: name.to_owned(),
                rva: entry.rva,
            });
            return;
        }
        let resolved = Resolved {
            rva: entry.rva,
            kind: entry.kind,
            origin: Origin::Cached,
        };
        self.symbols.insert(name.to_owned(), resolved);
    }

    /// Scan one symbol's patterns, taking the first that matches exactly once.
    fn scan(&mut self, image: &[u8], name: &str, entry: &crate::schema::PatternEntry) {
        let mut ambiguous = false;
        for pattern in &entry.patterns {
            let hits = pattern.find(image, 2);
            match hits.as_slice() {
                [] => {}
                [only] => match entry.resolve.apply(image, *only) {
                    Ok(rva) => {
                        let resolved = Resolved {
                            rva,
                            kind: SymbolKind::default(),
                            origin: Origin::Scanned,
                        };
                        self.symbols.insert(name.to_owned(), resolved);
                        return;
                    }
                    Err(e) => {
                        self.issues.push(Issue::BadResolve {
                            name: name.to_owned(),
                            reason: e.to_string(),
                        });
                        return;
                    }
                },
                _ => ambiguous = true,
            }
        }
        self.issues.push(if ambiguous {
            Issue::Ambiguous {
                name: name.to_owned(),
            }
        } else {
            Issue::NoMatch {
                name: name.to_owned(),
            }
        });
    }

    /// A resolved symbol.
    #[must_use]
    pub fn symbol(&self, name: &str) -> Option<Resolved> {
        self.symbols.get(name).copied()
    }

    /// A struct field offset.
    #[must_use]
    pub fn offset(&self, name: &str) -> Option<u64> {
        self.offsets.get(name).copied()
    }

    /// Every resolved symbol, sorted by name.
    pub fn symbols(&self) -> impl Iterator<Item = (&str, Resolved)> {
        self.symbols
            .iter()
            .map(|(name, resolved)| (name.as_str(), *resolved))
    }

    /// Every offset, sorted by name.
    pub fn offsets(&self) -> impl Iterator<Item = (&str, u64)> {
        self.offsets
            .iter()
            .map(|(name, value)| (name.as_str(), *value))
    }

    /// Everything that needs a person's attention.
    #[must_use]
    pub fn issues(&self) -> &[Issue] {
        &self.issues
    }

    /// Of `names`, those that did not resolve. Used to refuse a plugin whose requirements
    /// are not met, with the missing names in the message.
    #[must_use]
    pub fn missing<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
        names
            .into_iter()
            .filter(|name| !self.symbols.contains_key(*name))
            .collect()
    }

    /// Record that a symbol was asked for but is in neither half of the database.
    pub fn note_unknown(&mut self, name: &str) {
        if self
            .issues
            .iter()
            .any(|i| matches!(i, Issue::Unknown { name: n } if n == name))
        {
            return;
        }
        self.issues.push(Issue::Unknown {
            name: name.to_owned(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{BuildInfo, OffsetEntry, PatternEntry, Provenance};
    use crate::{Resolve, SCHEMA_VERSION};

    fn image() -> Vec<u8> {
        let mut image = vec![0x90; 0x200];
        image[0x100..0x104].copy_from_slice(&[0x48, 0x8B, 0xC4, 0x55]);
        image[0x180..0x184].copy_from_slice(&[0x41, 0x57, 0x48, 0x83]);
        image
    }

    fn build(symbols: Vec<(&str, SymbolEntry)>) -> BuildFile {
        BuildFile {
            schema: SCHEMA_VERSION,
            build: BuildInfo {
                version: "test".into(),
                executable: "DayZ_x64.exe".into(),
                sha256: Some("abc".into()),
                file_size: Some(1),
                pe_timestamp: None,
                image_size: None,
                verified: None,
                provenance: Provenance::Verified,
            },
            symbols: symbols
                .into_iter()
                .map(|(n, e)| (n.to_owned(), e))
                .collect(),
            offsets: [(
                "framebase.rotation".to_owned(),
                OffsetEntry {
                    value: 8,
                    note: None,
                },
            )]
            .into(),
        }
    }

    fn entry(rva: u64, check: Option<&str>) -> SymbolEntry {
        SymbolEntry {
            rva,
            kind: SymbolKind::Function,
            check: check.map(|c| c.parse().unwrap_or_else(|e| panic!("{e}"))),
            note: None,
        }
    }

    fn patterns(entries: Vec<(&str, PatternEntry)>) -> PatternFile {
        PatternFile {
            schema: SCHEMA_VERSION,
            symbols: entries
                .into_iter()
                .map(|(n, e)| (n.to_owned(), e))
                .collect(),
        }
    }

    fn pattern_entry(text: &str) -> PatternEntry {
        PatternEntry {
            patterns: vec![text.parse().unwrap_or_else(|e| panic!("{e}"))],
            resolve: Resolve::Direct,
            note: None,
            since: None,
        }
    }

    #[test]
    fn a_verified_cache_entry_is_used_and_offsets_come_through() {
        let image = image();
        let build = build(vec![("render.frame", entry(0x100, Some("48 8B C4")))]);
        let table = SymbolTable::resolve(&image, Some(&build), &PatternFile::default());
        assert_eq!(
            table.symbol("render.frame"),
            Some(Resolved {
                rva: 0x100,
                kind: SymbolKind::Function,
                origin: Origin::Cached
            })
        );
        assert_eq!(table.offset("framebase.rotation"), Some(8));
        assert!(table.issues().is_empty());
        assert_eq!(table.symbols().count(), 1);
        assert_eq!(table.offsets().count(), 1);
    }

    #[test]
    fn a_stale_cache_entry_is_discarded_rather_than_used() {
        let image = image();
        let build = build(vec![
            ("render.frame", entry(0x100, Some("55 55 55"))),
            ("render.world", entry(0x9999, None)),
        ]);
        let table = SymbolTable::resolve(&image, Some(&build), &PatternFile::default());
        assert_eq!(table.symbol("render.frame"), None);
        assert_eq!(table.symbol("render.world"), None);
        assert_eq!(
            table.issues(),
            [
                Issue::CheckFailed {
                    name: "render.frame".into(),
                    rva: 0x100
                },
                Issue::OutOfImage {
                    name: "render.world".into(),
                    rva: 0x9999
                },
            ]
        );
        assert!(table.issues()[0]
            .to_string()
            .contains("not the ones the database expects"));
    }

    #[test]
    fn scanning_covers_what_the_cache_does_not() {
        let image = image();
        let build = build(vec![("render.frame", entry(0x100, Some("48 8B C4")))]);
        let file = patterns(vec![
            ("render.frame", pattern_entry("41 57 48 83")),
            ("render.world", pattern_entry("41 57 48 83")),
        ]);
        let table = SymbolTable::resolve(&image, Some(&build), &file);
        assert_eq!(
            table.symbol("render.frame").map(|r| r.origin),
            Some(Origin::Cached),
            "cache wins"
        );
        assert_eq!(
            table.symbol("render.world"),
            Some(Resolved {
                rva: 0x180,
                kind: SymbolKind::Function,
                origin: Origin::Scanned
            })
        );
    }

    #[test]
    fn unmatched_and_ambiguous_patterns_are_reported() {
        let image = image();
        let file = patterns(vec![
            ("nowhere", pattern_entry("DE AD BE EF")),
            ("everywhere", pattern_entry("90 90")),
            (
                "multi",
                PatternEntry {
                    patterns: vec![
                        "DE AD".parse().unwrap_or_else(|e| panic!("{e}")),
                        "48 8B C4".parse().unwrap_or_else(|e| panic!("{e}")),
                    ],
                    ..pattern_entry("90")
                },
            ),
        ]);
        let table = SymbolTable::resolve(&image, None, &file);
        assert_eq!(table.symbol("nowhere"), None);
        assert_eq!(table.symbol("everywhere"), None);
        assert_eq!(
            table.symbol("multi").map(|r| r.rva),
            Some(0x100),
            "a later pattern can save it"
        );
        let names: Vec<&str> = table.issues().iter().map(Issue::name).collect();
        assert_eq!(names, ["everywhere", "nowhere"]);
        assert!(matches!(table.issues()[0], Issue::Ambiguous { .. }));
        assert!(matches!(table.issues()[1], Issue::NoMatch { .. }));
    }

    #[test]
    fn a_failing_resolution_rule_is_an_issue() {
        let image = image();
        let mut entry = pattern_entry("48 8B C4");
        entry.resolve = Resolve::Offset { offset: 0x1000 };
        let table = SymbolTable::resolve(&image, None, &patterns(vec![("bad", entry)]));
        assert_eq!(table.symbol("bad"), None);
        assert!(matches!(table.issues(), [Issue::BadResolve { .. }]));
    }

    #[test]
    fn missing_reports_requirements_and_unknowns_are_recorded_once() {
        let image = image();
        let build = build(vec![("render.frame", entry(0x100, None))]);
        let mut table = SymbolTable::resolve(&image, Some(&build), &PatternFile::default());
        assert_eq!(
            table.missing(["render.frame", "camera.manager", "gui.scale"]),
            ["camera.manager", "gui.scale"]
        );
        assert!(table.missing(["render.frame"]).is_empty());
        table.note_unknown("camera.manager");
        table.note_unknown("camera.manager");
        assert_eq!(
            table.issues(),
            [Issue::Unknown {
                name: "camera.manager".into()
            }]
        );
    }
}
