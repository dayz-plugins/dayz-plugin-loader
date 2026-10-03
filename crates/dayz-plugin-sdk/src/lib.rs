//! Safe Rust API for DayZ plugin loader plugins.
//!
//! Implement [`Plugin`], then `export_plugin!(MyPlugin);`. The macro generates the three C
//! exports the loader looks for, routes every callback through a panic guard and installs a
//! [`log`] backend that writes to the shared loader log.
//!
//! ```ignore
//! use dayz_plugin_sdk::{export_plugin, Host, Plugin, PluginError};
//!
//! struct Hello;
//!
//! impl Plugin for Hello {
//!     const NAME: &'static str = "hello";
//!     const VERSION: &'static str = env!("CARGO_PKG_VERSION");
//!     const DESCRIPTION: &'static str = "Says hello on every hotkey press.";
//!
//!     fn start(host: Host) -> Result<Self, PluginError> {
//!         host.hotkey("wave", "Wave", "f9")?;
//!         Ok(Hello)
//!     }
//!
//!     fn on_hotkey(&self, host: &Host, action: &str) {
//!         host.console_print(&format!("hello from {action}"));
//!     }
//! }
//!
//! export_plugin!(Hello);
//! ```

mod deps;
mod ffi;
mod host;
mod logger;
mod plugin;
mod settings;

pub use dayz_plugin_api as api;
pub use deps::Dependency;
pub use host::{Arg, CommandLine, Hook, Host, PluginError, PluginRef};
pub use plugin::{Plugin, PresentInfo, SwapchainInfo};
pub use settings::{Setting, SettingKind};

// Used by the macro expansion; not part of the public surface.
#[doc(hidden)]
pub mod __private {
    pub use crate::ffi::{describe, start, stop, Exports, Slot};
    pub use dayz_plugin_api::{HostApi, PluginCallbacks, PluginHandle, PluginInfo, Status};
}

/// Generate the loader exports for a [`Plugin`] implementation.
#[macro_export]
macro_rules! export_plugin {
    ($plugin:ty) => {
        static __DAYZ_PLUGIN_SLOT: $crate::__private::Slot<$plugin> =
            $crate::__private::Slot::new();

        /// Loader export: static description of this plugin.
        #[no_mangle]
        pub extern "C" fn dayz_plugin_describe() -> *const $crate::__private::PluginInfo {
            $crate::__private::describe::<$plugin>()
        }

        /// Loader export: create the plugin and publish its callback table.
        ///
        /// # Safety
        /// Called by the loader with a valid host table and writable callback table.
        #[no_mangle]
        pub unsafe extern "C" fn dayz_plugin_start(
            host: *const $crate::__private::HostApi,
            handle: $crate::__private::PluginHandle,
            callbacks: *mut $crate::__private::PluginCallbacks,
        ) -> $crate::__private::Status {
            // SAFETY: the loader guarantees `host` and `callbacks` are valid for this call.
            unsafe {
                $crate::__private::start::<$plugin>(&__DAYZ_PLUGIN_SLOT, host, handle, callbacks)
            }
        }

        /// Loader export: tear the plugin down.
        ///
        /// # Safety
        /// Called by the loader with the context from `dayz_plugin_start`.
        #[no_mangle]
        pub unsafe extern "C" fn dayz_plugin_stop(
            ctx: *mut ::core::ffi::c_void,
            reason: $crate::api::StopReason,
        ) {
            // SAFETY: `ctx` is the pointer `dayz_plugin_start` wrote into the callbacks.
            unsafe { $crate::__private::stop::<$plugin>(&__DAYZ_PLUGIN_SLOT, ctx, reason) }
        }

        const _: () = {
            fn __assert_exports<P: $crate::__private::Exports>() {}
            let _ = __assert_exports::<$plugin>;
        };
    };
}
