# dayz-plugin-loader

A plugin loader for DayZ, written in Rust. It ships as `dxgi.dll` next to `DayZ_x64.exe`,
forwards every DXGI call to the real system library, and hosts native plugins that the game
knows nothing about. Plugins are ordinary DLLs in `dayz-plugins/plugins/`; they are not PBO
mods and need no server support.

The loader gives every plugin:

- **Settings** that are typed, validated, persisted per plugin and changeable at runtime.
- **Hotkeys** with a readable binding grammar, user overrides, and no global key grabs: a
  hotkey only fires while the game window has focus.
- **A console** with built-in commands plus the commands and variables plugins register,
  including the plugin lifecycle commands.
- **Dependencies**, declared in the plugin and checked by the loader before it starts: other
  plugins with version requirements, libraries such as `openxr_loader.dll`, files, and
  `dayz-data` symbols. Dependencies also decide load order.
- **Plugin-to-plugin messaging**: direct messages with synchronous replies, and broadcast
  topics with subscriptions.
- **The process's command line and full environment**, parsed and ready to read.
- **Frame hooks**: swapchain creation, every `Present`, every `ResizeBuffers`, and a way to
  ask for a different backbuffer size before the swapchain exists.
- **Game addresses by name**, resolved from the
  [dayz-data](https://github.com/dayz-plugins/dayz-data) database, so no plugin carries an
  address of its own and a game update is a data change rather than a release of everything.
- **Isolation**: every call into a plugin is wrapped against panics and hardware faults. A
  plugin that faults is logged and disabled; the other plugins and the game keep running.

## Repository layout

| Crate | What it is |
| --- | --- |
| `crates/dayz-plugin-api` | The C ABI both sides share. `repr(C)`, no dependencies. |
| `crates/dayz-plugin-sdk` | Safe Rust API for writing plugins. Implement a trait, call one macro. |
| `crates/dayz-plugin-core` | Platform independent logic: key grammar, settings, console parsing, config files. Builds and tests on any host. |
| `crates/dayz-plugin-loader` | The `dxgi.dll` itself: exports, vtable hooks, plugin loading, host API. |
| `crates/dayz-data` | Reader and resolver for the address database: patterns, per-build caches, byte checks. |
| `tools/dayz-data-tool` | The `dayz-data` command: inspect an executable, validate a database, generate build files. |
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
    ├── data/patterns.json          the dayz-data database
    ├── data/builds/*.json          one file per known game build
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

### Dependencies

A plugin declares what it needs as a `const`, and the loader checks it before `start` runs:

```rust
const DEPENDENCIES: &'static [Dependency] = &[
    Dependency::plugin("dayz-vr", ">=0.2, <1"),   // also starts dayz-vr first
    Dependency::library("openxr_loader.dll"),     // findable, not loaded by the loader
    Dependency::file("dayz-plugins/data/hud.json"),
    Dependency::symbol("render.prepare_view"),
    Dependency::plugin("dayz-hud", "").optional(), // order only; missing is fine
];
```

A plugin whose mandatory dependency is missing is not started and the reason is one log line.
Plugin requirements are also a load order: dependencies start first, cycles are reported and
everyone in them stays unloaded, and a plugin that needed something that did not start does
not start either. One broken plugin never blocks a launch. Version requirements are comma
separated comparators (`>=`, `>`, `=`, `<`, `<=`; a bare version means `>=`) over dot
separated versions compared component by component, so `1.10` is newer than `1.9`.

`host.require_symbols(...)` inside `start` still exists and does the same for addresses; a
`Dependency::symbol` is the declarative form, checked before the plugin runs at all.

### Lifecycle from the console

| Command | Effect |
| --- | --- |
| `plugins`, `plugin list` | Loaded plugins, their version, file and whether they are running. |
| `plugin deps [name]` | What each plugin declared it needs. |
| `plugin load <name>` | Load, check and start a DLL from the plugin directory now. |
| `plugin stop <name>` | Call the plugin's stop export and stop delivering callbacks. |
| `plugin disable`/`enable <name>` | Pause and resume callback delivery without stopping. |

There is deliberately no reload. Unloading the DLL would mean `FreeLibrary` while the hooks
it installed, the threads it started and the pointers the loader and other plugins hold are
all still live, and a plugin name can only be used once per launch because handles are
indices that never move. Rebuild and restart the game; `plugin load` is for a DLL this session
has not seen.

Build it as a `cdylib` for `x86_64-pc-windows-msvc` and drop the DLL into
`dayz-plugins/plugins/`. See `examples/hello-plugin` for the complete crate.

A plugin in another language only needs the three exports and the structs from
`crates/dayz-plugin-api`; nothing in the ABI is Rust specific.

## Game addresses

A plugin asks for an address by name and never contains one:

```rust
host.require_symbols(["render.frame", "camera.manager"])?;   // in start, or stay unloaded
let frame = host.symbol("render.frame")?;
let rotation = host.offset("framebase.rotation")?;
```

The names resolve from [dayz-data](https://github.com/dayz-plugins/dayz-data), which the
loader reads at startup: the cache for a known executable, byte-pattern scanning for an
unknown one, with every cached address verified against the bytes it expects. The in-game
console's `symbols` command lists what resolved and what did not, and the log carries the
same per symbol. `require_symbols` is what makes a game update a named missing symbol in the
log instead of a crash.

The `dayz-data` command inspects and maintains the database:

```bash
cargo run -p dayz-data-tool -- validate ../dayz-data --exe "$DAYZ_DIR/DayZ_x64.exe"
```

## Documentation

Prose documentation lives in
[dayz-plugins.github.io](https://github.com/dayz-plugins/dayz-plugins.github.io): engine
reverse-engineering notes under `research/`, design documents under `design/`. This
repository carries only this README and `AGENTS.md`.

## License

Public domain (Unlicense), like the rest of the organisation's repositories.
