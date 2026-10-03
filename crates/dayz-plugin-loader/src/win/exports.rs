//! The DXGI exports the game (and Proton's d3d11) resolve from `dxgi.dll`, forwarded to the
//! system library after the loader had a chance to initialise and hook the factory.

// FFI module: exported C functions and dynamic symbol lookup.
#![allow(unsafe_code)]

use core::ffi::c_void;
use std::path::PathBuf;
use std::sync::OnceLock;

use windows::core::{s, Interface, GUID, HRESULT, PCWSTR};
use windows::Win32::Foundation::{E_FAIL, HMODULE, MAX_PATH};
use windows::Win32::Graphics::Dxgi::IDXGIFactory;
use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::SystemInformation::GetSystemDirectoryW;

use super::hooks;

type CreateFactoryFn = unsafe extern "system" fn(*const GUID, *mut *mut c_void) -> HRESULT;
type CreateFactory2Fn = unsafe extern "system" fn(u32, *const GUID, *mut *mut c_void) -> HRESULT;
type GetDebugInterfaceFn = unsafe extern "system" fn(u32, *const GUID, *mut *mut c_void) -> HRESULT;
type DeclareRemovalFn = unsafe extern "system" fn() -> HRESULT;

struct RealDxgi {
    create_factory: Option<CreateFactoryFn>,
    create_factory1: Option<CreateFactoryFn>,
    create_factory2: Option<CreateFactory2Fn>,
    get_debug_interface1: Option<GetDebugInterfaceFn>,
    declare_adapter_removal_support: Option<DeclareRemovalFn>,
}

static REAL: OnceLock<Option<RealDxgi>> = OnceLock::new();

/// Directory of the running executable.
pub(crate) fn game_dir() -> PathBuf {
    let mut buf = [0u16; MAX_PATH as usize];
    // SAFETY: null module means the executable; `buf` is writable for its length.
    let len = unsafe { GetModuleFileNameW(None, &mut buf) } as usize;
    let exe = PathBuf::from(String::from_utf16_lossy(&buf[..len.min(buf.len())]));
    exe.parent()
        .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf)
}

fn load_real() -> Option<RealDxgi> {
    let mut dir = [0u16; MAX_PATH as usize];
    // SAFETY: `dir` is writable for its length.
    let len = unsafe { GetSystemDirectoryW(Some(&mut dir)) } as usize;
    let mut path: Vec<u16> = dir[..len.min(dir.len())].to_vec();
    path.extend("\\dxgi.dll\0".encode_utf16());
    // SAFETY: `path` is NUL terminated and outlives the call.
    let module: HMODULE = match unsafe { LoadLibraryW(PCWSTR(path.as_ptr())) } {
        Ok(m) => m,
        Err(e) => {
            log::error!("cannot load system dxgi.dll: {e}");
            return None;
        }
    };
    // SAFETY: valid module handle and NUL terminated names; transmuting a FARPROC into the
    // documented export signature.
    unsafe {
        Some(RealDxgi {
            create_factory: GetProcAddress(module, s!("CreateDXGIFactory"))
                .map(|f| core::mem::transmute::<_, CreateFactoryFn>(f)),
            create_factory1: GetProcAddress(module, s!("CreateDXGIFactory1"))
                .map(|f| core::mem::transmute::<_, CreateFactoryFn>(f)),
            create_factory2: GetProcAddress(module, s!("CreateDXGIFactory2"))
                .map(|f| core::mem::transmute::<_, CreateFactory2Fn>(f)),
            get_debug_interface1: GetProcAddress(module, s!("DXGIGetDebugInterface1"))
                .map(|f| core::mem::transmute::<_, GetDebugInterfaceFn>(f)),
            declare_adapter_removal_support: GetProcAddress(
                module,
                s!("DXGIDeclareAdapterRemovalSupport"),
            )
            .map(|f| core::mem::transmute::<_, DeclareRemovalFn>(f)),
        })
    }
}

fn real() -> Option<&'static RealDxgi> {
    REAL.get_or_init(load_real).as_ref()
}

/// After a factory was created: hook it when the loader is active.
///
/// # Safety
/// `out` must hold the interface pointer the real call just returned.
unsafe fn after_factory(hr: HRESULT, out: *mut *mut c_void) -> HRESULT {
    if hr.is_ok() && !out.is_null() && super::initialize() {
        // SAFETY: `*out` is a live IUnknown-derived pointer owned by the caller; we only
        // borrow it to query the factory interface and never release it.
        let unknown = unsafe { windows::core::IUnknown::from_raw_borrowed(&*out) };
        if let Some(factory) = unknown.and_then(|u| u.cast::<IDXGIFactory>().ok()) {
            hooks::hook_factory(&factory);
        }
    }
    hr
}

/// `CreateDXGIFactory` export.
///
/// # Safety
/// Called by the OS loader with the documented DXGI signature.
#[no_mangle]
pub unsafe extern "system" fn CreateDXGIFactory(
    riid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    let Some(f) = real().and_then(|r| r.create_factory) else {
        return E_FAIL;
    };
    // SAFETY: forwarding the caller's arguments unchanged.
    let hr = unsafe { f(riid, out) };
    // SAFETY: `out` is the pointer the real call filled.
    unsafe { after_factory(hr, out) }
}

/// `CreateDXGIFactory1` export, the one DayZ imports.
///
/// # Safety
/// Called by the OS loader with the documented DXGI signature.
#[no_mangle]
pub unsafe extern "system" fn CreateDXGIFactory1(
    riid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    let Some(f) = real().and_then(|r| r.create_factory1) else {
        return E_FAIL;
    };
    // SAFETY: forwarding the caller's arguments unchanged.
    let hr = unsafe { f(riid, out) };
    // SAFETY: `out` is the pointer the real call filled.
    unsafe { after_factory(hr, out) }
}

/// `CreateDXGIFactory2` export.
///
/// # Safety
/// Called by the OS loader with the documented DXGI signature.
#[no_mangle]
pub unsafe extern "system" fn CreateDXGIFactory2(
    flags: u32,
    riid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    let Some(f) = real().and_then(|r| r.create_factory2) else {
        return E_FAIL;
    };
    // SAFETY: forwarding the caller's arguments unchanged.
    let hr = unsafe { f(flags, riid, out) };
    // SAFETY: `out` is the pointer the real call filled.
    unsafe { after_factory(hr, out) }
}

/// `DXGIGetDebugInterface1` export, forwarded unchanged.
///
/// # Safety
/// Called by the OS loader with the documented DXGI signature.
#[no_mangle]
pub unsafe extern "system" fn DXGIGetDebugInterface1(
    flags: u32,
    riid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    let Some(f) = real().and_then(|r| r.get_debug_interface1) else {
        return E_FAIL;
    };
    // SAFETY: forwarding the caller's arguments unchanged.
    unsafe { f(flags, riid, out) }
}

/// `DXGIDeclareAdapterRemovalSupport` export, forwarded unchanged.
///
/// # Safety
/// Called by the OS loader with the documented DXGI signature.
#[no_mangle]
pub unsafe extern "system" fn DXGIDeclareAdapterRemovalSupport() -> HRESULT {
    let Some(f) = real().and_then(|r| r.declare_adapter_removal_support) else {
        return E_FAIL;
    };
    // SAFETY: no arguments to forward.
    unsafe { f() }
}
