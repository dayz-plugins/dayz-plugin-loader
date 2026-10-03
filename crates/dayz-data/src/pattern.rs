//! Byte patterns: `48 8B C4 ?? 48 89 58 08`.
//!
//! A pattern is a sequence of bytes where `??` matches anything. That is the standard
//! notation every disassembler and signature tool uses, so patterns can be moved between
//! this database and other tooling without translation.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// Why a pattern string was rejected.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PatternError {
    /// No bytes at all.
    #[error("pattern is empty")]
    Empty,
    /// A token was neither two hexadecimal digits nor a wildcard.
    #[error("{0:?} is not a byte or a wildcard")]
    BadToken(String),
    /// The pattern begins or ends with a wildcard, which only widens it pointlessly.
    #[error("pattern must not begin or end with a wildcard")]
    EdgeWildcard,
}

/// A compiled byte pattern. `None` entries match any byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern(Vec<Option<u8>>);

impl Pattern {
    /// Number of bytes the pattern covers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the pattern covers no bytes. A parsed pattern never does.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether the pattern matches `haystack` starting exactly at `at`.
    #[must_use]
    pub fn matches_at(&self, haystack: &[u8], at: usize) -> bool {
        let Some(window) = haystack.get(at..at + self.0.len()) else {
            return false;
        };
        self.0
            .iter()
            .zip(window)
            .all(|(want, got)| want.is_none_or(|w| w == *got))
    }

    /// Every offset in `haystack` where the pattern matches, stopping after `limit` hits.
    ///
    /// The limit exists so that an over-broad pattern cannot make resolution quadratic: two
    /// hits already mean "ambiguous", and counting the rest of a million matches is waste.
    #[must_use]
    pub fn find(&self, haystack: &[u8], limit: usize) -> Vec<usize> {
        let mut hits = Vec::new();
        // The first byte is never a wildcard, so it can drive the search.
        let Some(Some(first)) = self.0.first().copied() else {
            return hits;
        };
        let last_start = haystack.len().saturating_sub(self.0.len());
        let mut at = 0;
        while at <= last_start {
            let Some(found) = haystack[at..=last_start].iter().position(|b| *b == first) else {
                break;
            };
            let candidate = at + found;
            if self.matches_at(haystack, candidate) {
                hits.push(candidate);
                if hits.len() >= limit {
                    break;
                }
            }
            at = candidate + 1;
        }
        hits
    }

    /// The single offset where the pattern matches, or `None` when it matches zero or
    /// several places. Ambiguity is a failure, not a reason to take the first hit: a pattern
    /// that matches twice is not identifying anything.
    #[must_use]
    pub fn find_unique(&self, haystack: &[u8]) -> Option<usize> {
        match self.find(haystack, 2).as_slice() {
            [only] => Some(*only),
            _ => None,
        }
    }
}

impl FromStr for Pattern {
    type Err = PatternError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut bytes = Vec::new();
        for token in text.split_whitespace() {
            if token.chars().all(|c| c == '?') {
                bytes.push(None);
                continue;
            }
            let byte = u8::from_str_radix(token, 16)
                .map_err(|_| PatternError::BadToken(token.to_owned()))?;
            bytes.push(Some(byte));
        }
        if bytes.is_empty() {
            return Err(PatternError::Empty);
        }
        if bytes.first() == Some(&None) || bytes.last() == Some(&None) {
            return Err(PatternError::EdgeWildcard);
        }
        Ok(Pattern(bytes))
    }
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, byte) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(" ")?;
            }
            match byte {
                Some(b) => write!(f, "{b:02X}")?,
                None => f.write_str("??")?,
            }
        }
        Ok(())
    }
}

impl Serialize for Pattern {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Pattern {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(text: &str) -> Pattern {
        text.parse().unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    #[test]
    fn parses_bytes_and_wildcards() {
        let p = pattern("48 8B ?? C4");
        assert_eq!(p.len(), 4);
        assert!(!p.is_empty());
        assert_eq!(p.to_string(), "48 8B ?? C4");
        assert_eq!(
            pattern("48 8b ? c4").to_string(),
            "48 8B ?? C4",
            "case and ? normalise"
        );
    }

    #[test]
    fn rejects_bad_patterns() {
        assert_eq!("".parse::<Pattern>(), Err(PatternError::Empty));
        assert_eq!(
            "48 zz".parse::<Pattern>(),
            Err(PatternError::BadToken("zz".into()))
        );
        assert_eq!("?? 48".parse::<Pattern>(), Err(PatternError::EdgeWildcard));
        assert_eq!("48 ??".parse::<Pattern>(), Err(PatternError::EdgeWildcard));
    }

    #[test]
    fn finds_matches_and_respects_wildcards() {
        let image = [0x00, 0x48, 0x8B, 0x01, 0xC4, 0x48, 0x8B, 0x99, 0xC4, 0x48];
        let p = pattern("48 8B ?? C4");
        assert_eq!(p.find(&image, 10), vec![1, 5]);
        assert_eq!(p.find(&image, 1), vec![1], "the limit stops the search");
        assert_eq!(p.find_unique(&image), None, "two hits identify nothing");
        assert_eq!(pattern("8B 01 C4").find_unique(&image), Some(2));
        assert!(pattern("48 8B 01 C5").find(&image, 4).is_empty());
    }

    #[test]
    fn a_pattern_longer_than_the_image_matches_nothing() {
        let image = [0x48, 0x8B];
        assert!(pattern("48 8B C4 90").find(&image, 4).is_empty());
        assert!(!pattern("48 8B").matches_at(&image, 1));
    }

    #[test]
    fn serde_round_trip() {
        let json = serde_json::to_string(&pattern("48 8B ?? C4")).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(json, "\"48 8B ?? C4\"");
        assert_eq!(
            serde_json::from_str::<Pattern>(&json).ok(),
            Some(pattern("48 8B ?? C4"))
        );
        assert!(serde_json::from_str::<Pattern>("\"nope\"").is_err());
    }
}
