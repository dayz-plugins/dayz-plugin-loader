//! Patch one COM vtable slot in place.

// FFI module: raw memory protection changes and pointer writes.
#![allow(unsafe_code)]

use core::ffi::c_void;

use windows::Win32::System::Memory::{
    VirtualProtect, PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS,
};

/// Replace the function pointer at `slot` with `replacement`, returning the previous value.
///
/// # Safety
/// `slot` must point at a vtable entry whose signature matches `replacement`. The caller
/// keeps the returned original and must call it with the same signature.
pub(crate) unsafe fn patch(
    slot: *mut *const c_void,
    replacement: *const c_void,
) -> Result<*const c_void, windows::core::Error> {
    let mut old = PAGE_PROTECTION_FLAGS(0);
    let size = core::mem::size_of::<*const c_void>();
    // SAFETY: `slot` is a valid pointer-sized location per the caller's contract.
    unsafe { VirtualProtect(slot.cast(), size, PAGE_EXECUTE_READWRITE, &raw mut old)? };
    // SAFETY: the page is writable now; replacing a pointer-sized value is atomic on x64.
    let previous = unsafe { slot.replace(replacement) };
    let mut ignored = PAGE_PROTECTION_FLAGS(0);
    // SAFETY: restoring the protection we recorded.
    unsafe { VirtualProtect(slot.cast(), size, old, &raw mut ignored)? };
    Ok(previous)
}
