# dayz-plugin-loader

A plugin loader for DayZ, written in Rust. It ships as `dxgi.dll` next to `DayZ_x64.exe`,
forwards every DXGI call to the real system library, and hosts native plugins that the game
knows nothing about. Plugins are ordinary DLLs in `plugins/`; they are not PBO
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
- **Hooks the loader owns**: byte patches, virtual table slots and inline detours registered
  through the host, so they are undone when the plugin stops, faults or the game exits.
- **An in-game overlay**: the loader owns `egui` and a Direct3D 11 renderer, so a plugin
  registers a panel and fills its body with widgets through the ABI, without ever touching the
  device. The loader's own console is drawn with it.
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

Or skip Steam's launch options entirely and start the game from this repository, which also
prints what is in `plugins/` and what will be ignored:

```bash
scripts/run-dayz.sh          # --console, every plugin; --stop closes it, --log follows the log
```

Loader flags go on the game's own command line:

| Flag | Effect |
| --- | --- |
| `--console` | Open a console window carrying the loader log, the in-game console and the game's own standard output, and reading typed commands. |
| `--noplugins` | Load no plugins at all, whatever the config says. |
| `--loader-log=<level>` | Override the log level for one run. |

All three also accept `-flag` and `/flag` spelling.

## Files in the game directory

```
DayZ/
├── dxgi.dll                        the loader
├── plugins/*.dll                   plugins, discovered in alphabetical order
└── plugin-loader/
    ├── config/loader.toml          loader settings
    ├── config/hotkeys.toml         hotkey overrides
    ├── config/windows.toml         where overlay windows were left, and their opacity
    ├── config/<plugin>.toml        one file per plugin, written by the loader
    ├── data/patterns.json          the dayz-data database
    ├── data/builds/*.json          one file per known game build
    └── logs/loader.log             current run; the previous one is loader.prev.log
```

`plugins/` is the only directory anything is dropped into by hand, which is why it sits in
the game folder rather than under `plugin-loader/`. A DLL in there that exports no
`dayz_plugin_describe` is ignored without being loaded at all, so a plugin's own dependency
or a leftover from another project cannot have its `DllMain` run by accident.

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
    Dependency::file("plugin-loader/data/hud.json"),
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

A plugin whose plugin dependency is missing is not rejected, it *waits*: `plugin list` shows
it as waiting, and it starts by itself the moment the dependency does. `plugin load` pulls in
missing dependencies first, so loading the top of a chain loads the chain.

There is deliberately no reload. Unloading the DLL would mean `FreeLibrary` while the threads
it started and the pointers the loader and other plugins hold are all still live, and a plugin
name can only be used once per launch because handles are indices that never move. Rebuild and
restart the game; `plugin load` is for a DLL this session has not seen.

### Lifecycle callbacks

| Callback | When |
| --- | --- |
| `start` | The plugin is created. The only place registrations are allowed. |
| `on_disable` | `plugin disable`: delivery is about to pause, state is kept. |
| `on_enable` | `plugin enable`: delivery resumed. |
| `stop(Unload)` | `plugin stop`: the game keeps running, so release everything. |
| `stop(Exit)` | The process is going away; do the least that is correct. |
| `stop(StartFailed)` | `start` failed partway and the loader is undoing it. |

### Panels

A plugin shows a window by registering a panel and filling its body once per frame. It never
links a UI library and never sees the device:

```rust
fn start(host: Host) -> Result<Self, PluginError> {
    host.panel("demo", "Hello plugin", false, "f10")?;   // "" for no key
    Ok(Hello)
}

fn on_ui(&self, host: &Host, ui: &Ui, _panel: &str) {
    ui.heading("Hello plugin");
    ui.label(&format!("{} frames", self.frames.load(Ordering::Relaxed)));
    ui.separator();
    let _ = ui.setting("greeting");       // the right control for a registered setting
    let _ = ui.setting("log_frames");
    if ui.button("Print the greeting") {
        host.console_print(&host.get("greeting").unwrap_or_default());
    }
}
```

`ui.setting(key)` is the one worth knowing: the loader reads the descriptor, draws the control
that fits the type, and writes a change back through the same path the console's `set` takes —
validated, persisted and `on_setting_changed` fired. A settings panel needs no state in the
plugin. Beside the control is a reset, offered once the value is no longer the default.

Which control a setting gets follows from its descriptor: a bounded range narrow enough to aim
at gets a slider, a wide one (a port is `1024..=65535`) gets a number field that still clamps,
and a setting marked `.advanced()` stays out of the settings editor until the user ticks
*Advanced settings*:

```rust
host.setting(
    &Setting::int("port", "Debug port", 48621, 1024, 65535)
        .with_description("Loopback TCP port dayz-ctl connects to.")
        .restart_required()
        .advanced(),
)?;
```

Advanced is only about that one list: `list`, `get`, `set` and `ui.setting` treat the setting
like any other.

The loader owns the window chrome, the open and closed state, the layout and the hotkey that
toggles the panel; the key never reaches `on_hotkey`. `host.set_panel_open` and
`host.panel_is_open` are there for a plugin that wants to drive its own window.

The widget token (`Ui`) is a number, not a pointer, and is only valid inside the `on_ui` call
that handed it over; keeping it gets a `WrongPhase`, not a dangling dereference. `on_ui` runs
on the render thread between the game's last draw call and its `Present`, so it must be short.

Every window that comes back has an id — `loader.console`, `loader.settings`, or
`<plugin>.<panel>` — and the loader remembers it by that id: its position, its size and its
opacity, in `windows.toml`, written when the mouse is let go rather than while a window is
being dragged. The title bar carries an opacity slider next to the close button, so a panel
that only needs watching can be faded over the game instead of covering it.

The loader's own console is drawn through the same machinery. `--console` still opens a
Windows console window; the key under Escape (`sc29` by default, rebindable as
`loader.console` in `hotkeys.toml`) opens the same console inside the game.

### The settings editor

`F11` (rebindable as `loader.settings`) opens one window holding every plugin's settings and
hotkeys. A plugin gets it for free: it registered a setting because it needed the value and a
hotkey because it needed the action, and neither costs it a line of UI.

Each setting is drawn as the control its type calls for, with a reset once it differs from the
default. Each hotkey has a recorder — click the binding, press the key — plus *Clear* and
*Default*. A recorded key is stored by name when the grammar has one (`f10`) and by position
otherwise (`sc29`), which is what makes the key under Escape bindable at all: under Wine it
reports a virtual key no layout table claims.

Everything the editor writes goes through the same calls the console makes, so a slider and a
typed `set` cannot disagree, and `hotkeys.toml` ends up holding exactly the overrides.

### Answering for the console

`host.console_exec` runs a line; its output goes to the console window and the log. A plugin
that is answering someone else's question — a remote console, an overlay, a test — needs the
lines themselves:

```rust
let (status, lines) = host.console_capture("plugins");
```

The sink is called once per printed line before the call returns, and the echoed `> line` is
not part of it. This is the whole mechanism behind
[dayz-debug-plugin](https://github.com/dayz-plugins/dayz-debug-plugin), which serves the
console on a loopback socket without knowing what a single command means.

### Input

The loader owns the game's window procedure, so it sees every key, every mouse message and
every `WM_INPUT` first. A plugin can watch that stream, take events out of it, and put its own
in:

```rust
host.listen_input(Watch::KEY | Watch::MOUSE_MOVE)?;   // any time, not only in start

fn on_input(&self, _host: &Host, input: &Input<'_>) -> Verdict {
    match input {
        Input::MouseMove { dx, dy, raw: true, .. } => {
            self.look(*dx, *dy);
            Verdict::SWALLOW          // the game does not see this one
        }
        _ => Verdict::PASS,
    }
}
```

```rust
// Sending: real system input, in one burst, so a chord arrives as a chord.
host.send_input(&[Action::key_down(0x57), Action::mouse_move(12, 0), Action::key_up(0x57)])?;
host.send_input(&[Action::scancode(0x11, true)])?;    // by position, which DayZ binds on
let sprinting = host.key_down(0x10);                  // VK_SHIFT, right now
host.register_hid(0x01, 0x05)?;                       // raw reports from gamepads
```

`SendInput` rather than a message posted to the window, because DayZ reads the mouse through
raw input and the keyboard through a polled table and neither notices a synthesised
`WM_KEYDOWN`. The consequence worth knowing: sent input comes back around through `on_input`
like anything else, so a plugin that both sends and watches sees its own.

Three rules hold the stream together:

- **The overlay wins.** While a loader window has the keyboard, neither plugins nor the game
  get the event — someone typing a console command is not aiming.
- **The first swallow ends delivery.** Two plugins cannot each believe they own an event. The
  console's `input` command lists who is watching and how much has been taken.
- **A kind nobody asked for costs nothing.** The window procedure checks one atomic before it
  builds an event, which is why `Watch::NONE` is worth passing when a plugin is done.

`on_input` runs inside the game's message loop, for every matching event. It must return
immediately; a plugin with real work to do hands it to its own thread. A plugin that faults in
there is disabled like anywhere else, and the event passes.

This exists for [dayz-vr-plugin](https://github.com/dayz-plugins/dayz-vr-plugin), which turns
head and controller motion into input the game already understands, and
[dayz-dinput-plugin](https://github.com/dayz-plugins/dayz-dinput-plugin), which reads devices
the game never asked the system for.

### Hooks

A plugin can patch the game, but registering the hook through the loader means the loader
holds the original and puts it back:

```rust
// SAFETY: the caller is patching the game; the loader checks the address, not the intent.
let hook = unsafe { host.patch(address, &[0x90; 5], "skip the retail check")? };
let (hook, original) = unsafe { host.hook_vtable(swapchain, 8, my_present, "present")? };
let (hook, trampoline) = unsafe { host.detour(target, my_fn, "camera update")? };
host.remove_hook(hook)?;                 // or let the loader do it on stop
```

The loader refuses an address that is not committed memory and one that another hook already
holds, undoes a plugin's hooks in reverse order when it stops, and lists them all in the
console's `hooks` command. A detour steals whole instructions, refuses a prologue it cannot
relocate (anything instruction-pointer-relative or branching), and jumps through a relay page
allocated near the target so the patch stays five bytes. The game's other threads are not
suspended while that jump is written, so install hooks from `start` or `on_swapchain` rather
than mid-frame.

Build it as a `cdylib` for `x86_64-pc-windows-msvc` and drop the DLL into
`plugins/`. See `examples/hello-plugin` for the complete crate.

A plugin in another language only needs the three exports and the structs from
`crates/dayz-plugin-api`; nothing in the ABI is Rust specific.

The ABI carries a version (`API_VERSION`, currently 6) and every struct its own `struct_size`,
so the loader refuses a plugin built against a different version rather than reading a shorter
table. Fields are only ever appended; a version bump means an existing field changed meaning.
Version 2 added dependencies and the lifecycle callbacks, version 3 the hook registry and
`console_capture`, version 4 the UI panels and `on_ui`, version 5 the toasts, notices and
dialogs, version 6 the input stream and `on_input`. A plugin and the loader it runs in must
come from the same version, which in practice means rebuilding plugins when the loader's ABI
moves.

Two of the input types — `InputResponse` and `InputActionKind` — are transparent structs with
constants rather than enums, because those two travel from the plugin to the loader. A plugin
built against a later ABI could hold a value this loader has never heard of, and reading that
into a Rust enum would be undefined behaviour instead of something the loader can refuse.
`InputKind` stays an enum: it travels the other way, and a kind added later has a mask bit an
older plugin never sets, so it is never delivered to one.

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

### Looking at what a symbol points at

`read` dumps memory from the console, which is how a candidate symbol gets identified before
anything is built on it:

```
read camera.manager              # 64 bytes at the symbol
read *camera.manager 128         # follow the pointer stored there, then dump 128 bytes
read *engine.singleton+18 32     # follow, add 0x18, dump 32
read 0x7ff6c21a4400              # by address, when the symbol has no name yet
```

Every read is checked before it happens — `VirtualQuery` has to report the page as committed
and readable, the length is clamped to the end of that region, and the copy still runs inside
the fault guard, so a page that stops being readable in between prints a line instead of
taking the game down. Nothing here writes.

The `dayz-data` command inspects and maintains the database:

```bash
cargo run -p dayz-data-tool -- validate ../dayz-data --exe "$DAYZ_DIR/DayZ_x64.exe"
```

## Related repositories

| Repository | What it is |
| --- | --- |
| [dayz-data](https://github.com/dayz-plugins/dayz-data) | The address database: patterns, per-build caches, seeds. |
| [dayz-patches-plugin](https://github.com/dayz-plugins/dayz-patches-plugin) | Workarounds for engine bugs, one switch each |
| [dayz-debug-plugin](https://github.com/dayz-plugins/dayz-debug-plugin) | The console, settings and state on a loopback socket, plus `dayz-ctl` to talk to it from a shell. |
| [dayz-plugins.github.io](https://github.com/dayz-plugins/dayz-plugins.github.io) | All prose documentation: research notes and design documents. |

## Documentation

Prose documentation lives in
[dayz-plugins.github.io](https://github.com/dayz-plugins/dayz-plugins.github.io): engine
reverse-engineering notes under `research/`, design documents under `design/`. This
repository carries only this README and `AGENTS.md`.

## License

Public domain (Unlicense), like the rest of the organisation's repositories.
