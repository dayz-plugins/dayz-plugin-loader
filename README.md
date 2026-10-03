# dayz-plugin-loader

A plugin loader for DayZ, written in Rust. It ships as `dxgi.dll` next to `DayZ_x64.exe`,
forwards every DXGI call to the real system library, and hosts native plugins that the game
knows nothing about. Plugins are ordinary DLLs in `dayz-plugins/plugins/`; they are not PBO
mods and need no server support.

The loader gives every plugin:

- **Settings** that are typed, validated, persisted per plugin and changeable at runtime.
- **Hotkeys** with a readable binding grammar, user overrides, and no global key grabs: a
  hotkey only fires while the game window has focus.
- **A console** with built-in commands plus the commands and variables plugins register.
- **Plugin-to-plugin messaging**: direct messages with synchronous replies, and broadcast
  topics with subscriptions.
- **The process's command line and full environment**, parsed and ready to read.
- **Frame hooks**: swapchain creation, every `Present`, every `ResizeBuffers`, and a way to
  ask for a different backbuffer size before the swapchain exists.
- **Isolation**: every call into a plugin is wrapped against panics and hardware faults. A
  plugin that faults is logged and disabled; the other plugins and the game keep running.

## Repository layout

| Crate | What it is |
| --- | --- |
| `crates/dayz-plugin-api` | The C ABI both sides share. `repr(C)`, no dependencies. |
| `crates/dayz-plugin-sdk` | Safe Rust API for writing plugins. Implement a trait, call one macro. |
| `crates/dayz-plugin-core` | Platform independent logic: key grammar, settings, console parsing, config files. Builds and tests on any host. |
| `crates/dayz-plugin-loader` | The `dxgi.dll` itself: exports, vtable hooks, plugin loading, host API. |
| `examples/hello-plugin` | Smallest useful plugin, and the SDK's smoke test. |

## Building

The host is an immutable OS, so everything compiles inside the `build-box` container. The
loader targets `x86_64-pc-windows-msvc`, because only MSVC provides the structured exception
handling that contains a faulting plugin.

```bash
scripts/build.sh
```

`--check` runs the host gate only, `--fast` skips it, and `--deploy` installs into
`$DAYZ_DIR` (default: the Steam library path).

## Running

The game must be told to prefer the local `dxgi.dll` over the system one:

```bash
WINEDLLOVERRIDES="dxgi=n,b" %command%
```

Loader flags go on the game's own command line:

| Flag | Effect |
| --- | --- |
| `--console` | Open a console window carrying the loader log, the in-game console and the game's own standard output. |
| `--noplugins` | Load no plugins at all, whatever the config says. |
| `--loader-log=<level>` | Override the log level for one run. |

All three also accept `-flag` and `/flag` spelling.

## Files in the game directory

```
DayZ/
├── dxgi.dll                        the loader
└── dayz-plugins/
    ├── plugins/*.dll               plugins, loaded in alphabetical order
    ├── config/loader.toml          loader settings
    ├── config/hotkeys.toml         hotkey overrides
    ├── config/<plugin>.toml        one file per plugin, written by the loader
    └── logs/loader.log             current run; the previous one is loader.prev.log
```

## Writing a plugin

```rust
use dayz_plugin_sdk::{export_plugin, Host, Plugin, PluginError, Setting};

struct Hello;

impl Plugin for Hello {
    const NAME: &'static str = "hello";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const DESCRIPTION: &'static str = "Greets on a hotkey.";

    fn start(host: Host) -> Result<Self, PluginError> {
        host.setting(&Setting::text("greeting", "Greeting", "Hello"))?;
        host.hotkey("greet", "Print the greeting", "f9")?;
        host.command("greet", "Print the greeting.", "[name]")?;
        Ok(Hello)
    }

    fn on_hotkey(&self, host: &Host, _action: &str) {
        host.console_print(&host.get("greeting").unwrap_or_default());
    }
}

export_plugin!(Hello);
```

Build it as a `cdylib` for `x86_64-pc-windows-msvc` and drop the DLL into
`dayz-plugins/plugins/`. See `examples/hello-plugin` for the complete crate.

A plugin in another language only needs the three exports and the structs from
`crates/dayz-plugin-api`; nothing in the ABI is Rust specific.

## License

Public domain (Unlicense), like the rest of the organisation's repositories.
