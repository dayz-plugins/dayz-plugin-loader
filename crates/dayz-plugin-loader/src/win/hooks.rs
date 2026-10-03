//! Vtable hooks on the DXGI factory and swapchain.
//!
//! Two layers: the factory's creation functions, so the loader sees the swapchain being
//! made and can rewrite its size, and the swapchain's `Present` and `ResizeBuffers`. COM
//! vtables are shared per class, so each is patched exactly once and every instance is
//! covered.

// FFI module: raw vtable manipulation and calls through function pointers.
#![allow(unsafe_code)]

use core::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use dayz_plugin_api::{PresentInfo, SwapchainInfo};
use windows::core::{Interface, HRESULT};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT;
use windows::Win32::Graphics::Dxgi::{
    IDXGIFactory, IDXGIFactory2, IDXGISwapChain, DXGI_SWAP_CHAIN_DESC, DXGI_SWAP_CHAIN_DESC1,
};

use super::{dispatch, state, ui, vtable};

/// Vtable slot numbers, counted from the start of the interface's vtable.
mod slot {
    /// `IDXGIFactory::CreateSwapChain`.
    pub(super) const CREATE_SWAP_CHAIN: usize = 10;
    /// `IDXGIFactory2::CreateSwapChainForHwnd`.
    pub(super) const CREATE_SWAP_CHAIN_FOR_HWND: usize = 15;
    /// `IDXGISwapChain::Present`.
    pub(super) const PRESENT: usize = 8;
    /// `IDXGISwapChain::ResizeBuffers`.
    pub(super) const RESIZE_BUFFERS: usize = 13;
}

type CreateSwapChainFn = unsafe extern "system" fn(
    *mut c_void,
    *mut c_void,
    *mut DXGI_SWAP_CHAIN_DESC,
    *mut *mut c_void,
) -> HRESULT;
type CreateForHwndFn = unsafe extern "system" fn(
    *mut c_void,
    *mut c_void,
    HWND,
    *const DXGI_SWAP_CHAIN_DESC1,
    *const c_void,
    *mut c_void,
    *mut *mut c_void,
) -> HRESULT;
type PresentFn = unsafe extern "system" fn(*mut c_void, u32, u32) -> HRESULT;
type ResizeBuffersFn =
    unsafe extern "system" fn(*mut c_void, u32, u32, u32, DXGI_FORMAT, u32) -> HRESULT;

/// The function a vtable slot held before it was patched.
///
/// Stored as an atomic pointer rather than a `static mut`: a detour runs on whichever thread
/// the game calls it from, possibly while another thread is still installing the next hook,
/// and an atomic makes that read well defined. A null pointer means "not hooked yet".
struct Original(AtomicPtr<c_void>);

impl Original {
    const fn new() -> Self {
        Original(AtomicPtr::new(core::ptr::null_mut()))
    }

    /// Record the function the slot held before it was patched.
    fn store(&self, previous: *const c_void) {
        self.0.store(previous.cast_mut(), Ordering::Release);
    }

    /// The original, reinterpreted as a function of type `F`.
    ///
    /// # Safety
    /// `F` must be the exact signature of the function whose slot was patched.
    unsafe fn get<F: Copy>(&self) -> Option<F> {
        let ptr = self.0.load(Ordering::Acquire);
        if ptr.is_null() {
            return None;
        }
        debug_assert_eq!(
            core::mem::size_of::<F>(),
            core::mem::size_of::<*const c_void>(),
            "F must be a plain function pointer"
        );
        // SAFETY: the pointer came out of a vtable slot holding a function of type `F`, per
        // this function's contract, and a function pointer has the same layout as a pointer.
        Some(unsafe { core::mem::transmute_copy::<*mut c_void, F>(&ptr) })
    }
}

static REAL_CREATE_SWAP_CHAIN: Original = Original::new();
static REAL_CREATE_FOR_HWND: Original = Original::new();
static REAL_PRESENT: Original = Original::new();
static REAL_RESIZE_BUFFERS: Original = Original::new();

static FACTORY_HOOKED: AtomicBool = AtomicBool::new(false);
static SWAPCHAIN_HOOKED: AtomicBool = AtomicBool::new(false);

/// Address of vtable entry `index` of a COM object.
///
/// # Safety
/// `object` must be a live COM interface pointer with at least `index + 1` vtable entries.
unsafe fn entry(object: *mut c_void, index: usize) -> *mut *const c_void {
    // SAFETY: a COM object starts with a pointer to its vtable, per the caller's contract.
    let table = unsafe { *object.cast::<*mut *const c_void>() };
    // SAFETY: the interface is documented to have this many entries.
    unsafe { table.add(index) }
}

/// Patch the factory's creation functions. Safe to call for every factory the game makes.
pub(crate) fn hook_factory(factory: &IDXGIFactory) {
    if FACTORY_HOOKED.swap(true, Ordering::SeqCst) {
        return;
    }
    let raw = factory.as_raw();
    // SAFETY: `raw` is a live IDXGIFactory, whose vtable has the slot we patch.
    let result = unsafe {
        vtable::patch(
            entry(raw, slot::CREATE_SWAP_CHAIN),
            create_swap_chain as *const c_void,
        )
    };
    match result {
        Ok(previous) => {
            REAL_CREATE_SWAP_CHAIN.store(previous);
            log::info!("hooked IDXGIFactory::CreateSwapChain");
        }
        Err(e) => log::error!("could not hook CreateSwapChain: {e}"),
    }
    if let Ok(factory2) = factory.cast::<IDXGIFactory2>() {
        let raw2 = factory2.as_raw();
        // SAFETY: `raw2` is a live IDXGIFactory2, whose vtable has the slot we patch.
        let result = unsafe {
            vtable::patch(
                entry(raw2, slot::CREATE_SWAP_CHAIN_FOR_HWND),
                create_for_hwnd as *const c_void,
            )
        };
        match result {
            Ok(previous) => {
                REAL_CREATE_FOR_HWND.store(previous);
                log::info!("hooked IDXGIFactory2::CreateSwapChainForHwnd");
            }
            Err(e) => log::error!("could not hook CreateSwapChainForHwnd: {e}"),
        }
    }
}

/// Patch `Present` and `ResizeBuffers`, then tell the plugins about the swapchain.
fn hook_swapchain(raw: *mut c_void) {
    if raw.is_null() || SWAPCHAIN_HOOKED.swap(true, Ordering::SeqCst) {
        return;
    }
    for (index, replacement, original, name) in [
        (
            slot::PRESENT,
            present as *const c_void,
            &REAL_PRESENT,
            "Present",
        ),
        (
            slot::RESIZE_BUFFERS,
            resize_buffers as *const c_void,
            &REAL_RESIZE_BUFFERS,
            "ResizeBuffers",
        ),
    ] {
        // SAFETY: `raw` is a live IDXGISwapChain, whose vtable has both slots.
        match unsafe { vtable::patch(entry(raw, index), replacement) } {
            Ok(previous) => {
                original.store(previous);
                log::info!("hooked IDXGISwapChain::{name}");
            }
            Err(e) => log::error!("could not hook {name}: {e}"),
        }
    }
}

/// Announce a new swapchain to the plugins.
fn announce(raw: *mut c_void, hwnd: *mut c_void, width: u32, height: u32) {
    super::set_game_window(hwnd);
    ui::on_swapchain(hwnd);
    let device = device_of(raw);
    let info = SwapchainInfo {
        struct_size: core::mem::size_of::<SwapchainInfo>(),
        swapchain: raw,
        device,
        hwnd,
        width,
        height,
    };
    dispatch::swapchain(&info);
}

/// The `ID3D11Device` behind a swapchain, or null when it cannot be queried.
fn device_of(raw: *mut c_void) -> *mut c_void {
    // SAFETY: `raw` is a live IDXGISwapChain pointer we only borrow.
    let chain = unsafe { IDXGISwapChain::from_raw_borrowed(&raw) };
    let Some(chain) = chain else {
        return core::ptr::null_mut();
    };
    // SAFETY: `chain` is a live swapchain; `GetDevice` only queries an interface from it.
    let device = unsafe { chain.GetDevice::<windows::Win32::Graphics::Direct3D11::ID3D11Device>() };
    device.map_or_else(|_| core::ptr::null_mut(), |d| d.as_raw())
}

unsafe extern "system" fn create_swap_chain(
    this: *mut c_void,
    device: *mut c_void,
    desc: *mut DXGI_SWAP_CHAIN_DESC,
    out: *mut *mut c_void,
) -> HRESULT {
    // SAFETY: the slot this pointer came from held a function of type `CreateSwapChainFn`.
    let real = unsafe { REAL_CREATE_SWAP_CHAIN.get::<CreateSwapChainFn>() };
    let Some(real) = real else { return HRESULT(-1) };
    let mut hwnd = core::ptr::null_mut();
    if !desc.is_null() {
        // SAFETY: the caller owns a writable descriptor for the duration of the call.
        let desc = unsafe { &mut *desc };
        hwnd = desc.OutputWindow.0;
        if let Some((width, height)) = state().backbuffer_override {
            log::info!(
                "swapchain {}x{} overridden to {width}x{height}",
                desc.BufferDesc.Width,
                desc.BufferDesc.Height
            );
            desc.BufferDesc.Width = width;
            desc.BufferDesc.Height = height;
        }
    }
    // SAFETY: forwarding the caller's arguments to the original function.
    let hr = unsafe { real(this, device, desc, out) };
    if hr.is_ok() && !out.is_null() {
        // SAFETY: `out` holds the swapchain the real call just created.
        let created = unsafe { *out };
        hook_swapchain(created);
        // SAFETY: the descriptor is still valid here.
        let (w, h) =
            unsafe { desc.as_ref() }.map_or((0, 0), |d| (d.BufferDesc.Width, d.BufferDesc.Height));
        announce(created, hwnd, w, h);
    }
    hr
}

unsafe extern "system" fn create_for_hwnd(
    this: *mut c_void,
    device: *mut c_void,
    hwnd: HWND,
    desc: *const DXGI_SWAP_CHAIN_DESC1,
    fullscreen: *const c_void,
    restrict: *mut c_void,
    out: *mut *mut c_void,
) -> HRESULT {
    // SAFETY: the slot this pointer came from held a function of type `CreateForHwndFn`.
    let real = unsafe { REAL_CREATE_FOR_HWND.get::<CreateForHwndFn>() };
    let Some(real) = real else { return HRESULT(-1) };
    // SAFETY: the caller owns a readable descriptor for the duration of the call.
    let original = unsafe { desc.as_ref() }.copied();
    let patched = original.map(|mut d| {
        if let Some((width, height)) = state().backbuffer_override {
            log::info!(
                "swapchain {}x{} overridden to {width}x{height}",
                d.Width,
                d.Height
            );
            d.Width = width;
            d.Height = height;
        }
        d
    });
    let desc_ptr = patched.as_ref().map_or(desc, core::ptr::from_ref);
    // SAFETY: forwarding the caller's arguments, with a descriptor copy of the same layout.
    let hr = unsafe { real(this, device, hwnd, desc_ptr, fullscreen, restrict, out) };
    if hr.is_ok() && !out.is_null() {
        // SAFETY: `out` holds the swapchain the real call just created.
        let created = unsafe { *out };
        hook_swapchain(created);
        let (w, h) = patched.map_or((0, 0), |d| (d.Width, d.Height));
        announce(created, hwnd.0, w, h);
    }
    hr
}

unsafe extern "system" fn present(this: *mut c_void, sync_interval: u32, flags: u32) -> HRESULT {
    // SAFETY: the slot this pointer came from held a function of type `PresentFn`.
    let real = unsafe { REAL_PRESENT.get::<PresentFn>() };
    let Some(real) = real else { return HRESULT(-1) };
    super::tick(this);
    let info = PresentInfo {
        struct_size: core::mem::size_of::<PresentInfo>(),
        swapchain: this,
        sync_interval,
        flags,
    };
    dispatch::present(&info);
    // Last, so the overlay draws over whatever the plugins drew this frame.
    ui::present(this);
    // SAFETY: forwarding the caller's arguments to the original function.
    unsafe { real(this, sync_interval, flags) }
}

unsafe extern "system" fn resize_buffers(
    this: *mut c_void,
    buffer_count: u32,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
    flags: u32,
) -> HRESULT {
    // SAFETY: the slot this pointer came from held a function of type `ResizeBuffersFn`.
    let real = unsafe { REAL_RESIZE_BUFFERS.get::<ResizeBuffersFn>() };
    let Some(real) = real else { return HRESULT(-1) };
    // SAFETY: forwarding the caller's arguments to the original function.
    let hr = unsafe { real(this, buffer_count, width, height, format, flags) };
    if hr.is_ok() {
        dispatch::resize(width, height);
    }
    hr
}
