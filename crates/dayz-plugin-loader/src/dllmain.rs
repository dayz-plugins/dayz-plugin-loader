//! Entry point. Everything real happens when the game calls a DXGI export; `DllMain` only
//! records the process-detach moment so plugins get a chance to stop cleanly.

// The entry point is an OS callback with a fixed signature.
#![allow(unsafe_code)]

use core::ffi::c_void;

use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::System::LibraryLoader::DisableThreadLibraryCalls;
use windows::Win32::System::SystemServices::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH};

/// `TRUE` as the Win32 `BOOL` this entry point returns.
const TRUE: i32 = 1;

/// Windows entry point.
///
/// # Safety
/// Called by the OS loader with the documented arguments.
#[no_mangle]
pub unsafe extern "system" fn DllMain(
    module: HINSTANCE,
    reason: u32,
    _reserved: *mut c_void,
) -> i32 {
    match reason {
        DLL_PROCESS_ATTACH => {
            // No plugin work here: `DllMain` runs under the loader lock, where loading
            // another DLL can deadlock. Initialisation happens on the first DXGI call.
            // SAFETY: `module` is this DLL's handle, as the OS passed it.
            let _ = unsafe { DisableThreadLibraryCalls(module.into()) };
        }
        DLL_PROCESS_DETACH => crate::win::shutdown(),
        _ => {}
    }
    TRUE
}
