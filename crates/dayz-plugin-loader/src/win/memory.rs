//! The console's `read` command: a guarded look at the game's memory.
//!
//! This is the tool for finding out what a symbol actually points at while the game runs —
//! which is how the game-state sources a plugin event would need get identified in the first
//! place. It is deliberately read-only, and it checks before it reads: `VirtualQuery` says
//! whether the page is committed and readable, the length is clamped to the end of that
//! region, and the copy itself still happens inside the fault guard, because a page can stop
//! being readable between the question and the answer.

// FFI module: queries and a raw copy out of the game's address space.
#![allow(unsafe_code)]

use core::ffi::c_void;
use std::fmt::Write as _;

use windows::Win32::System::Memory::{
    VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE,
    PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY,
};

use super::{data, guard};

/// Bytes dumped when the line does not say.
const DEFAULT_LEN: usize = 64;
/// Most bytes one `read` will print, so a typo cannot flood the console.
const MAX_LEN: usize = 1024;
/// Bytes per dump line.
const PER_LINE: usize = 16;

/// Page protections that allow a read. A region with any other protection is left alone.
const READABLE: [u32; 6] = [
    PAGE_READONLY.0,
    PAGE_READWRITE.0,
    PAGE_WRITECOPY.0,
    PAGE_EXECUTE_READ.0,
    PAGE_EXECUTE_READWRITE.0,
    PAGE_EXECUTE_WRITECOPY.0,
];

/// What a `read` target named, before anything was resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    /// Read the pointer stored at the address and continue from there.
    deref: bool,
    /// Symbol name, or `0x…` address.
    base: String,
    /// Added after the optional dereference.
    offset: u64,
}

/// Parse `[*]<symbol|0xaddr>[+offset]`. The offset is hex, with or without `0x`.
fn parse(target: &str) -> Result<Target, String> {
    let text = target.trim();
    let (deref, rest) = match text.strip_prefix('*') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (base, offset) = match rest.split_once('+') {
        Some((base, offset)) => {
            let digits = offset
                .trim()
                .trim_start_matches("0x")
                .trim_start_matches("0X");
            let value = u64::from_str_radix(digits, 16)
                .map_err(|_| format!("{offset:?} is not a hex offset"))?;
            (base.trim(), value)
        }
        None => (rest.trim(), 0),
    };
    if base.is_empty() {
        return Err("read what? give a symbol name or an address".to_owned());
    }
    Ok(Target {
        deref,
        base: base.to_owned(),
        offset,
    })
}

/// Resolve a target's base to an address in this process.
fn base_address(base: &str) -> Result<u64, String> {
    if let Some(digits) = base.strip_prefix("0x").or_else(|| base.strip_prefix("0X")) {
        return u64::from_str_radix(digits, 16).map_err(|_| format!("{base:?} is not an address"));
    }
    let Some(resolved) = data::resolved() else {
        return Err("no symbol database is loaded; read by address instead".to_owned());
    };
    let name = base.to_ascii_lowercase();
    if let Some(symbol) = resolved.table.symbol(&name) {
        return Ok(resolved.module_base as u64 + symbol.rva);
    }
    if resolved.table.offset(&name).is_some() {
        return Err(format!(
            "{name} is a struct field offset, not an address; add it to one: read <symbol>+<hex>"
        ));
    }
    Err(format!("no symbol named {name}; try: symbols {name}"))
}

/// How many readable bytes are available at `address`, up to `wanted`.
///
/// `None` when the address is not committed, readable memory. A guard page counts as not
/// readable: touching it is what the game's own stack growth uses it for.
fn readable(address: u64, wanted: usize) -> Option<usize> {
    let mut info = MEMORY_BASIC_INFORMATION::default();
    let size = core::mem::size_of::<MEMORY_BASIC_INFORMATION>();
    // SAFETY: `info` is writable and `size` describes it; querying an arbitrary address is
    // what VirtualQuery is for and it reports rather than faults.
    let written = unsafe { VirtualQuery(Some(address as *const c_void), &raw mut info, size) };
    if written == 0 || info.State != MEM_COMMIT {
        return None;
    }
    if info.Protect.0 & PAGE_GUARD.0 != 0 || !READABLE.contains(&(info.Protect.0 & 0xFF)) {
        return None;
    }
    let end = info.BaseAddress as u64 + info.RegionSize as u64;
    let room = usize::try_from(end.saturating_sub(address)).unwrap_or(usize::MAX);
    Some(wanted.min(room)).filter(|room| *room > 0)
}

/// Copy `len` bytes from `address`, inside the fault guard.
fn copy(address: u64, len: usize) -> Result<Vec<u8>, String> {
    let mut bytes = vec![0_u8; len];
    let destination = bytes.as_mut_ptr();
    // SAFETY: the region was just reported as committed and readable, `destination` owns
    // `len` writable bytes, and the copy runs under the structured handler, which is what
    // covers the gap between the query and the read.
    let outcome = unsafe {
        guard::call(move || {
            core::ptr::copy_nonoverlapping(address as *const u8, destination, len);
        })
    };
    match outcome {
        Ok(()) => Ok(bytes),
        Err(fault) => Err(format!("{address:#018x} faulted while reading: {fault}")),
    }
}

/// One dump line: offset, hex bytes, then the printable characters.
fn dump_line(address: u64, bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(PER_LINE * 3);
    for (i, byte) in bytes.iter().enumerate() {
        if i == PER_LINE / 2 {
            hex.push(' ');
        }
        let _ = write!(hex, "{byte:02x} ");
    }
    let text: String = bytes
        .iter()
        .map(|b| {
            let c = char::from(*b);
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '.'
            }
        })
        .collect();
    format!("{address:#018x}  {hex:<49}|{text}|")
}

/// What the first eight bytes look like if read as one value.
fn as_word(bytes: &[u8]) -> Option<String> {
    let word = u64::from_le_bytes(bytes.get(..8)?.try_into().ok()?);
    let note = if readable(word, 1).is_some() {
        let module = data::resolved().map_or(0, |r| r.module_base as u64);
        if module != 0 && word > module {
            format!("readable, module+{:#x}", word - module)
        } else {
            "readable".to_owned()
        }
    } else {
        "not a readable address".to_owned()
    };
    Some(format!("first qword {word:#018x} ({word}) — {note}"))
}

/// Lines for the console's `read` command.
pub(crate) fn console_lines(target: &str, count: Option<usize>) -> Vec<String> {
    let wanted = count.unwrap_or(DEFAULT_LEN).clamp(1, MAX_LEN);
    let parsed = match parse(target) {
        Ok(parsed) => parsed,
        Err(e) => return vec![e],
    };
    let mut lines = Vec::new();
    let mut address = match base_address(&parsed.base) {
        Ok(address) => address,
        Err(e) => return vec![e],
    };
    if parsed.deref {
        let Some(len) = readable(address, 8) else {
            return vec![format!("{address:#018x} is not readable memory")];
        };
        if len < 8 {
            return vec![format!("{address:#018x} has no room for a pointer")];
        }
        let pointer = match copy(address, 8) {
            Ok(bytes) => u64::from_le_bytes(bytes.try_into().unwrap_or([0; 8])),
            Err(e) => return vec![e],
        };
        lines.push(format!("{address:#018x} -> {pointer:#018x}"));
        address = pointer;
    }
    address = address.wrapping_add(parsed.offset);
    let Some(len) = readable(address, wanted) else {
        lines.push(format!("{address:#018x} is not committed, readable memory"));
        return lines;
    };
    if len < wanted {
        lines.push(format!(
            "only {len} bytes readable here, {wanted} asked for"
        ));
    }
    let bytes = match copy(address, len) {
        Ok(bytes) => bytes,
        Err(e) => {
            lines.push(e);
            return lines;
        }
    };
    if let Some(note) = as_word(&bytes) {
        lines.push(note);
    }
    for (i, chunk) in bytes.chunks(PER_LINE).enumerate() {
        lines.push(dump_line(address + (i * PER_LINE) as u64, chunk));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_part_of_a_target() {
        assert_eq!(
            parse("camera.manager"),
            Ok(Target {
                deref: false,
                base: "camera.manager".to_owned(),
                offset: 0
            })
        );
        assert_eq!(
            parse("*engine.singleton+18"),
            Ok(Target {
                deref: true,
                base: "engine.singleton".to_owned(),
                offset: 0x18
            })
        );
        assert_eq!(parse("0x7ff6abcd+0x20").map(|t| t.offset), Ok(0x20));
        assert!(parse("*").is_err());
        assert!(parse("name+zz").is_err());
    }

    #[test]
    fn a_dump_line_shows_bytes_and_text() {
        let line = dump_line(0x1000, b"DayZ\0\x01");
        assert!(
            line.starts_with("0x0000000000001000  44 61 79 5a "),
            "{line}"
        );
        assert!(line.ends_with("|DayZ..|"), "{line}");
    }
}
