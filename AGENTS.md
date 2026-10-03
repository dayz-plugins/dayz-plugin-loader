# Agent notes: dayz-plugin-loader

Rules and context for anyone, human or agent, working in this repository.

## Repository hygiene

- **No `docs/` folder here.** Prose documentation lives in
  [dayz-plugins.github.io](https://github.com/dayz-plugins/dayz-plugins.github.io):
  engine reverse-engineering notes under `research/`, design documents under `design/`.
  Code repositories carry `README.md` and `AGENTS.md` and nothing else of that kind.
- Rust doc comments are not documentation files; write them freely. `cargo doc` is part of
  the gate.
- Everything that can be written in Rust is written in Rust.

## Build and verification

The host is an immutable OS with no toolchain, so every build runs in the `build-box`
container. `scripts/build.sh` wires the whole gate together and is the thing to run:

```bash
scripts/build.sh            # format, clippy, tests, docs, Windows clippy, release DLLs
scripts/build.sh --check    # host gate only, seconds
scripts/build.sh --deploy   # also install into $DAYZ_DIR
```

- **Never filter build, lint or test output** through `grep`, `head` or `tail`. Narrow the
  command instead.
- `--workspace --all-targets` is not optional for clippy; without it member crates' warnings
  stay invisible.
- Lint configuration lives only in `[workspace.lints]` in the root `Cargo.toml`. A member
  crate that repeats it carries a second copy that drifts.
- A suppression needs the specific lint named and a comment saying why. Report every one.

## Architecture, and the reasons behind it

| Crate | Role |
| --- | --- |
| `dayz-plugin-api` | The C ABI. `repr(C)`, no dependencies, `struct_size` on anything that may grow. |
| `dayz-plugin-core` | Platform independent logic. No Windows, so it tests on the host in seconds. |
| `dayz-plugin-sdk` | Safe Rust API for plugin authors. |
| `dayz-plugin-loader` | The `dxgi.dll`: exports, vtable hooks, plugin loading, host API. |
| `dayz-data` | Address database reader and resolver. Platform independent, so it tests on the host. |
| `dayz-data-tool` | The `dayz-data` command. Needs PE parsing, which is why it is separate from the reader. |

Things that look like style choices but are load-bearing:

- **The target is `x86_64-pc-windows-msvc`.** Only MSVC has the `__try` that `microseh` needs,
  and that structured exception handler is what turns a faulting plugin into a log line
  instead of a dead game. A mingw build compiles but contains panics only, and says so at
  startup.
- **`panic = "unwind"` in the release profile.** `abort` would defeat the panic guard.
- **The state mutex is never held across a call into a plugin.** State methods return the
  notifications to deliver (`state::Notify`); the caller delivers them after unlocking. This
  is what lets a plugin change a setting from inside `on_present` without deadlocking.
- **The plugin list is append only, published through an atomic pointer.** Dispatch needs no
  lock at all on the render thread; disabling a faulted plugin flips an atomic, and `plugin
  load` publishes a new vector and leaks the old one rather than mutating the one a render
  thread may be walking. Nothing is ever removed: a `PluginHandle` is an index.
- **Plugins are never unloaded.** `FreeLibrary` with hooks, threads and borrowed pointers live
  is not something a stop export can make safe, so `plugin stop` calls the stop export and
  silences the plugin while its DLL stays resident. Do not add a reload that pretends
  otherwise.
- **Loading is two passes:** describe every DLL, then let the declared dependencies decide who
  starts and in what order (`dayz_plugin_core::deps` for the graph, `win/deps.rs` for the
  libraries, files and symbols). A rejection is a log line, never an aborted launch, and a
  plugin waiting for another plugin stays described in `plugins::PENDING` so it can start
  later; anything that cannot change within a launch is refused instead of parked.
- **A DLL is only loaded if its bytes contain `dayz_plugin_describe`.** `plugins/` is in the
  game folder and collects other people's DLLs; `LoadLibrary` on one of those would run its
  `DllMain`. The export name check costs a file read and avoids that entirely.
- **Hooks plugins install go through `win/plugin_hooks.rs`.** The loader holds the original
  bytes, vtable pointer or stolen prologue and restores them when the plugin stops, faults or
  the game exits. `win/detour.rs` owns the inline-detour mechanics: `iced-x86` for instruction
  lengths, a relay page allocated near the target so the patch is five bytes, trampoline and
  relay pages deliberately never freed. It refuses an instruction-pointer-relative or
  branching prologue rather than relocating it wrongly, and it does **not** suspend the game's
  threads while writing the jump: that is a known gap, documented in the README.
- **`retour` is not usable here.** It pulls in libudis86 as C, which does not cross-compile
  under clang-cl (`-includestring.h` is a gcc-ism and `memset` ends up undeclared). That is
  why the detour is written on top of a pure-Rust decoder instead.
- **Hotkeys are polled from `Present` and gated on window focus.** `RegisterHotKey` was tried
  in the predecessor project: under this Wayland setup the registration succeeds and the key
  never fires, and a global grab is not wanted anyway.
- **`DllMain` does almost nothing.** It runs under the OS loader lock, where loading another
  DLL can deadlock, so initialisation happens on the first DXGI call.
- Settings are stored as text and validated against the plugin's descriptor on read, so a
  hand-edited config file can never desynchronise a type.
- **Addresses are data, never constants.** They live in the
  [dayz-data](https://github.com/dayz-plugins/dayz-data) repository and resolve by name at
  startup. Never compile an address into a crate here, and never let a plugin do it: that is
  the whole reason the database exists. Patterns are the source of truth and the per-build
  file is a cache, so the common case after a game update is that scanning finds everything
  and nobody has to do anything.
- **A global never gets a byte check.** Its bytes on disk are initialisation data, not what
  memory holds at runtime, so a check over one fails on every launch.

## Conventions

- Conventional Commits, imperative subject. One revertable commit per fix or feature.
- Commit trailer: `Co-Authored-By: <model name> <noreply@anthropic.com>`.
- Push to `origin` (`github.com/dayz-plugins/dayz-plugin-loader`).
- GitHub Actions never run under this account: keep `workflow_dispatch` live and leave every
  automatic trigger commented out with a note saying why.
- Source files stay under 600 lines where possible, 1000 hard; the limit is enforced by
  `crates/dayz-plugin-core/tests/size_limits.rs`, which walks every crate.
- Functions under 50 lines, enforced by clippy's `too_many_lines`.

## Known leftovers

`scripts/*.py`, `scripts/ghidra*` and `tools/config-editor/` came from the predecessor C++
project and still target its `dayz_openxr.ini`. They are kept for the Ghidra tooling until
the replacement exists; do not treat them as current.
