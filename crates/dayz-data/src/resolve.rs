//! What to do with a pattern hit.
//!
//! A signature rarely sits on the thing you want. It usually matches an instruction that
//! *references* it: a `lea` or `mov` with a RIP-relative operand pointing at a global, or a
//! `call` whose operand points at a function. These variants turn a match offset into the
//! address the symbol actually names.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Why a hit could not be turned into an address.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ResolveError {
    /// The operand the rule reads lies outside the image.
    #[error("operand at +{offset} of the match is outside the image")]
    OutOfBounds {
        /// Offset within the match that the rule wanted to read.
        offset: usize,
    },
    /// The computed target lies outside the image.
    #[error("resolved address {target:#X} is outside the image")]
    TargetOutOfBounds {
        /// The address that did not land inside the image.
        target: u64,
    },
}

/// How to get from a pattern hit to the symbol's address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Resolve {
    /// The match offset is the address. The default.
    #[default]
    Direct,
    /// Add a fixed number of bytes to the match offset, for a pattern that deliberately
    /// starts earlier than the symbol because the earlier bytes are what make it unique.
    Offset {
        /// Bytes to add.
        offset: i64,
    },
    /// Read a 32-bit signed displacement at `offset` within the match and follow it as an
    /// x86-64 RIP-relative reference. `length` is the length of the whole instruction, which
    /// is what RIP points past; it defaults to `offset + 4`, correct whenever the
    /// displacement is the instruction's last field.
    RipRelative {
        /// Offset of the displacement within the match.
        offset: usize,
        /// Instruction length, when the displacement is not its last field.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        length: Option<usize>,
    },
}

impl Resolve {
    /// Apply the rule to a hit at `at` within `image`, returning an image-relative address.
    ///
    /// # Errors
    /// The operand or the result lies outside the image.
    pub fn apply(self, image: &[u8], at: usize) -> Result<u64, ResolveError> {
        let target = match self {
            Resolve::Direct => at as u64,
            Resolve::Offset { offset } => add(at as u64, offset),
            Resolve::RipRelative { offset, length } => {
                let start = at + offset;
                let bytes = image
                    .get(start..start + 4)
                    .ok_or(ResolveError::OutOfBounds { offset })?;
                let mut displacement = [0u8; 4];
                displacement.copy_from_slice(bytes);
                let next_instruction = at + length.unwrap_or(offset + 4);
                add(
                    next_instruction as u64,
                    i64::from(i32::from_le_bytes(displacement)),
                )
            }
        };
        if target >= image.len() as u64 {
            return Err(ResolveError::TargetOutOfBounds { target });
        }
        Ok(target)
    }
}

/// Add a signed delta to an address, saturating rather than wrapping so a bad rule produces
/// an out-of-bounds error instead of a plausible-looking wrong address.
fn add(base: u64, delta: i64) -> u64 {
    if delta >= 0 {
        base.saturating_add(delta.unsigned_abs())
    } else {
        base.saturating_sub(delta.unsigned_abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `48 8B 0D 10 00 00 00` is `mov rcx, [rip+0x10]` at offset 0: seven bytes long, so RIP
    /// is 7 and the target is 7 + 0x10 = 0x17.
    fn image() -> Vec<u8> {
        let mut image = vec![0x90; 0x40];
        image[0..7].copy_from_slice(&[0x48, 0x8B, 0x0D, 0x10, 0x00, 0x00, 0x00]);
        image
    }

    #[test]
    fn direct_and_fixed_offset() {
        let image = image();
        assert_eq!(Resolve::Direct.apply(&image, 0x10), Ok(0x10));
        assert_eq!(Resolve::Offset { offset: 8 }.apply(&image, 0x10), Ok(0x18));
        assert_eq!(Resolve::Offset { offset: -8 }.apply(&image, 0x10), Ok(0x08));
    }

    #[test]
    fn rip_relative_follows_the_displacement() {
        let image = image();
        let rule = Resolve::RipRelative {
            offset: 3,
            length: None,
        };
        assert_eq!(rule.apply(&image, 0), Ok(0x17));
        let explicit = Resolve::RipRelative {
            offset: 3,
            length: Some(7),
        };
        assert_eq!(explicit.apply(&image, 0), Ok(0x17));
    }

    #[test]
    fn negative_displacements_resolve_backwards() {
        let mut image = vec![0x90; 0x40];
        image[0x20..0x27].copy_from_slice(&[0x48, 0x8B, 0x0D, 0xF0, 0xFF, 0xFF, 0xFF]);
        let rule = Resolve::RipRelative {
            offset: 3,
            length: None,
        };
        assert_eq!(rule.apply(&image, 0x20), Ok(0x17));
    }

    #[test]
    fn out_of_range_results_are_errors_not_wrong_addresses() {
        let image = image();
        let rule = Resolve::RipRelative {
            offset: 3,
            length: None,
        };
        assert_eq!(
            rule.apply(&image, 0x3E),
            Err(ResolveError::OutOfBounds { offset: 3 })
        );
        assert_eq!(
            Resolve::Offset { offset: 0x1000 }.apply(&image, 0),
            Err(ResolveError::TargetOutOfBounds { target: 0x1000 })
        );
        assert_eq!(Resolve::Offset { offset: -0x1000 }.apply(&image, 0), Ok(0));
    }

    #[test]
    fn direct_is_the_default_and_serialises_by_kind() {
        assert_eq!(Resolve::default(), Resolve::Direct);
        let json = serde_json::to_string(&Resolve::RipRelative {
            offset: 3,
            length: None,
        })
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(json, "{\"kind\":\"rip_relative\",\"offset\":3}");
        let parsed: Resolve = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            parsed,
            Resolve::RipRelative {
                offset: 3,
                length: None
            }
        );
    }
}
