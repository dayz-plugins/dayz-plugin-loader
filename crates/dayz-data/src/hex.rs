//! Serde helpers for addresses written as `"0x8E77C0"`.
//!
//! Hexadecimal is how every tool and note spells an address, and a JSON number would both
//! lose that spelling and invite a decimal typo that still parses.

use serde::{Deserialize, Deserializer, Serializer};

/// Parse `0x…`, `…h` or a plain hexadecimal string.
fn parse(text: &str) -> Option<u64> {
    let trimmed = text.trim();
    let body = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    u64::from_str_radix(body, 16).ok()
}

/// Deserialize a `u64` written as a hexadecimal string, or as a plain JSON number.
pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Text(String),
        Number(u64),
    }
    match Either::deserialize(d)? {
        Either::Number(n) => Ok(n),
        Either::Text(t) => parse(&t)
            .ok_or_else(|| serde::de::Error::custom(format!("{t:?} is not a hexadecimal address"))),
    }
}

/// Serialize a `u64` as `"0x…"`, upper case, which is how the notes spell addresses.
// The reference is not a choice: `serde(with = ...)` requires exactly this signature.
#[allow(clippy::trivially_copy_pass_by_ref)]
pub(crate) fn serialize<S: Serializer>(value: &u64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&format!("{value:#X}").replace("0X", "0x"))
}

/// The same mapping for an optional field.
pub(crate) mod option {
    use serde::{Deserialize, Deserializer, Serializer};

    /// Deserialize `Option<u64>` from a hexadecimal string, a number, or null.
    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
        #[derive(Deserialize)]
        struct Wrapper(#[serde(with = "super")] u64);
        Ok(Option::<Wrapper>::deserialize(d)?.map(|w| w.0))
    }

    /// Serialize `Option<u64>` as a hexadecimal string, or null.
    // The reference is not a choice: `serde(with = ...)` requires exactly this signature.
    #[allow(clippy::ref_option)]
    pub(crate) fn serialize<S: Serializer>(value: &Option<u64>, s: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(v) => super::serialize(v, s),
            None => s.serialize_none(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::Serialize;

    #[derive(serde::Deserialize, Serialize, PartialEq, Eq, Debug)]
    struct Holder {
        #[serde(with = "super")]
        rva: u64,
    }

    #[test]
    fn accepts_the_spellings_notes_use() {
        for text in ["0x8E77C0", "0X8e77c0", "8E77C0", " 0x8E77C0 "] {
            let json = format!("{{\"rva\":\"{text}\"}}");
            let parsed: Holder =
                serde_json::from_str(&json).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(parsed.rva, 0x008E_77C0);
        }
        let from_number: Holder =
            serde_json::from_str("{\"rva\":9336768}").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(from_number.rva, 0x008E_77C0);
    }

    #[test]
    fn round_trips_as_hex_text() {
        let holder = Holder { rva: 0x008E_77C0 };
        let json = serde_json::to_string(&holder).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(json, "{\"rva\":\"0x8E77C0\"}");
        assert_eq!(serde_json::from_str::<Holder>(&json).ok(), Some(holder));
    }

    #[test]
    fn rejects_nonsense() {
        assert!(serde_json::from_str::<Holder>("{\"rva\":\"0xzz\"}").is_err());
        assert!(serde_json::from_str::<Holder>("{\"rva\":\"\"}").is_err());
    }
}
