//! Reading the game's own memory without trusting it.
//!
//! Everything here is a read of an address that came out of the game, which means any of them
//! could be a pointer that is not one. So every read asks the system first whether the pages
//! are committed and readable, and returns an absence rather than taking a fault. The cost is
//! a `VirtualQuery` per read; the alternative is a crash in the middle of a frame, in a hook,
//! in somebody else's game.
//!
//! The one structure worth describing is Enfusion's string holder, `enf::BasicStringHolder`:
//!
//! ```text
//! +0x00  u32  reference count
//! +0x08  u64  length, INCLUDING the terminator — the allocator stores strlen + 1
//! +0x10  ...  the characters
//! ```
//!
//! and an empty string is a null pointer, not a holder holding nothing. Both of those were
//! learned the hard way; see [`engine_string`].

// FFI module: reads the game's objects through raw pointers.
#![allow(unsafe_code)]

use core::ffi::c_void;

use dayz_data::SymbolTable;
use windows::Win32::System::Memory::{
    VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_GUARD, PAGE_NOACCESS,
    PAGE_PROTECTION_FLAGS,
};

/// Longest NUL terminated string read out of the game before giving up on it.
pub(super) const MAX_NAME: usize = 128;

/// Longest holder string read out of the game. Generous; a length past this is a holder
/// pointer that is not one.
pub(super) const MAX_STRING: usize = 64 * 1024;

/// Offsets from `dayz-data` that reading the game's objects needs.
pub(super) struct Layout {
    /// Byte offset of an event's name getter in its virtual table.
    pub(super) vtable_name: usize,
    /// Byte offset of the length in a string holder.
    pub(super) string_length: usize,
    /// Byte offset of the characters in a string holder.
    pub(super) string_data: usize,
}

impl Layout {
    /// Read the three offsets, or say which one is missing.
    pub(super) fn resolve(table: &SymbolTable) -> Result<Layout, &'static str> {
        let get = |name: &str| table.offset(name).and_then(|v| usize::try_from(v).ok());
        Ok(Layout {
            vtable_name: get("event.vtable_name").ok_or("event.vtable_name")?,
            string_length: get("string.length").ok_or("string.length")?,
            string_data: get("string.data").ok_or("string.data")?,
        })
    }
}

/// Whether `len` bytes from `address` are committed memory this process may read.
pub(super) fn readable(address: usize, len: usize) -> bool {
    if address == 0 || len == 0 {
        return false;
    }
    let mut info = MEMORY_BASIC_INFORMATION::default();
    let size = core::mem::size_of::<MEMORY_BASIC_INFORMATION>();
    // SAFETY: `info` is writable and `size` describes it; querying any address is allowed.
    let written = unsafe { VirtualQuery(Some(address as *const c_void), &raw mut info, size) };
    if written != size || info.State != MEM_COMMIT {
        return false;
    }
    if info.Protect & (PAGE_NOACCESS | PAGE_GUARD) != PAGE_PROTECTION_FLAGS(0) {
        return false;
    }
    address
        .checked_add(len)
        .is_some_and(|end| end <= info.BaseAddress as usize + info.RegionSize)
}

/// One pointer from the game's memory, if it is there to read.
pub(super) fn read_pointer(address: usize) -> Option<usize> {
    if !readable(address, core::mem::size_of::<usize>()) {
        return None;
    }
    // SAFETY: the address holds one committed, readable pointer, checked above.
    Some(unsafe { (address as *const usize).read_unaligned() })
}

/// Four bytes from the game's memory, if they are there to read.
pub(super) fn read_u32(address: usize) -> Option<u32> {
    if !readable(address, 4) {
        return None;
    }
    // SAFETY: four committed, readable bytes, checked immediately above.
    Some(unsafe { (address as *const u32).read_unaligned() })
}

/// One `f32` from the game's memory, if it is there to read.
pub(super) fn read_f32(address: usize) -> Option<f32> {
    read_u32(address).map(f32::from_bits)
}

/// A NUL terminated string from the game's memory, at most [`MAX_NAME`] bytes.
pub(super) fn read_c_string(address: usize) -> Option<Box<str>> {
    if !readable(address, 1) {
        return None;
    }
    let mut bytes = Vec::new();
    for step in 0..MAX_NAME {
        let at = address.checked_add(step)?;
        if !readable(at, 1) {
            return None;
        }
        // SAFETY: one committed, readable byte, checked immediately above.
        let byte = unsafe { (at as *const u8).read() };
        if byte == 0 {
            return String::from_utf8(bytes).ok().map(String::into_boxed_str);
        }
        bytes.push(byte);
    }
    None
}

/// One string out of an engine holder, copied. A null holder is an empty string: that is how
/// the engine represents one, so it has to read as one here.
pub(super) fn engine_string(holder: *mut *mut c_void, layout: &Layout) -> String {
    let Some(holder) = read_pointer(holder as usize) else {
        return String::new();
    };
    holder_string(holder, layout)
}

/// The same, from the holder's own address rather than from a pointer to it.
pub(super) fn holder_string(holder: usize, layout: &Layout) -> String {
    if holder == 0 {
        return String::new();
    }
    let Some(length) = holder
        .checked_add(layout.string_length)
        .and_then(read_pointer)
        .filter(|length| *length <= MAX_STRING)
    else {
        return String::new();
    };
    let Some(data) = holder.checked_add(layout.string_data) else {
        return String::new();
    };
    if !readable(data, length) {
        return String::new();
    }
    // SAFETY: `length` bytes from `data` are committed and readable, checked above, and
    // `length` came from the holder's own length field rather than from a scan.
    let bytes = unsafe { core::slice::from_raw_parts(data as *const u8, length) };
    String::from_utf8_lossy(cut_at_nul(bytes)).into_owned()
}

/// The characters of a holder, without its terminator.
///
/// The length field counts the terminator — the allocator stores `strlen + 1` and copies that
/// many bytes — so the characters are one short of it. Cutting at the first NUL rather than
/// just dropping the last byte, because that is right either way: if a build ever stored the
/// bare `strlen` this still returns the text instead of appending whatever follows it.
fn cut_at_nul(bytes: &[u8]) -> &[u8] {
    bytes.split(|byte| *byte == 0).next().unwrap_or(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The holder's length field counts the terminator, which a live session caught: the
    /// first chat line through the hook came out as `Survivor\0` because this read one byte
    /// too many. The slicing is pinned down here so it holds without a game.
    #[test]
    fn a_holders_characters_stop_at_the_terminator() {
        let cut = |bytes: &[u8]| String::from_utf8_lossy(cut_at_nul(bytes)).into_owned();
        // What the engine actually stores: "Survivor" with length 9, the terminator included.
        assert_eq!(cut(b"Survivor\0"), "Survivor");
        // And if a build ever stored the bare strlen, the text is still right.
        assert_eq!(cut(b"Survivor"), "Survivor");
        // An empty holder, which the engine writes as a null pointer rather than this.
        assert_eq!(cut(b"\0"), "");
        assert_eq!(cut(b""), "");
    }

    #[test]
    fn nothing_is_readable_at_a_null_or_empty_range() {
        assert!(!readable(0, 8));
        assert!(!readable(0x1000, 0));
        assert!(read_pointer(0).is_none());
        assert!(read_u32(0).is_none());
        assert!(read_f32(0).is_none());
        assert!(read_c_string(0).is_none());
    }

    #[test]
    fn a_null_holder_is_the_empty_string() {
        let layout = Layout {
            vtable_name: 0x10,
            string_length: 0x08,
            string_data: 0x10,
        };
        assert_eq!(holder_string(0, &layout), "");
    }
}
