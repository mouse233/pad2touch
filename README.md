# Pad2Touch

**Turn a Windows Precision Touchpad into a touchscreen.**

Pad2Touch reads absolute finger positions from your touchpad through Windows Raw Input and replays them as native multitouch input (`InjectTouchInput`) on a window or region of your screen. It runs entirely in user mode — no driver, no hooks into the target application.

> **Status:** experimental. Expect rough edges; settings are not saved between launches yet.

## Features

- Up to 5 simultaneous contacts, delivered as real `PT_TOUCH` events (not mouse emulation)
- Map the touchpad to the whole screen, a monitor, a dragged portion of the screen, or an application window (optionally just part of it) — window mappings follow the window as it moves or resizes
- Touchpad area modes: Force proportions, Full, and a user-selected Portion
- A Wacom-style diagram shows your screens, the touchpad and the mapped region
- Native GUI with device picker, Start/Stop, and global Toggle / Hold shortcuts
- Built-in diagnostic view with per-finger trails and device info (size, DPI, report rate)
- Separate touchpad and touchscreen system-gesture suppression while capturing, both enabled by default

## Requirements

- Windows x64 (AMD64) or Windows ARM64 with a Precision Touchpad
- To build from source, install stable Rust with the MSVC toolchain, Visual C++ build tools, and a Windows SDK for your architecture.

## Getting started

### Download a build

Open the latest successful [CI run for `main`](https://github.com/mouse233/pad2touch/actions/workflows/ci.yml?query=branch%3Amain+is%3Asuccess). Under **Artifacts**, choose `pad2touch-windows-x64-*` for x64 or `pad2touch-windows-arm64-*` for ARM64. Extract the ZIP and run `Pad2Touch.exe`. CI artifacts are available for seven days.

### Build from source

Run these commands with the native Rust MSVC toolchain for your Windows architecture.

```powershell
cargo build --release --locked
.\target\release\Pad2Touch.exe
```

The GUI opens idle. Pick a touchpad, a **Screen area** and a **Touchpad** mode, then click **Start capture**. You can also enable Toggle (Ctrl+Alt+F8–F11) or Hold (Alt). The first valid touch is injected directly, with up to five simultaneous contacts and Windows touch feedback enabled by default. Selected system-gesture overrides last until capture stops, then restore the original settings.

**Ctrl+Alt+Esc** stops capture from anywhere.

Command-line modes (probe, raw capture, self-test, …) are available through the helper script:

```powershell
.\scripts\Run-Pad2Touch.ps1 -Mode probe
.\scripts\Run-Pad2Touch.ps1 -Mode capture -Inject -Contacts 5 -Seconds 120
```

## Limitations

- Windows routes injected touches by hit testing, so the target must stay visible. Touches over a covering window are dropped.
- Apps running elevated (as administrator) may reject the input.
- The touchpad keeps acting as a mouse at the same time; Raw Input cannot capture it exclusively.

See the [GUI guide](docs/gui.md) and [architecture notes](docs/architecture.md) for the full list.

## Development

```powershell
cargo test --locked
cargo clippy --all-targets -- -D warnings
cargo fmt -- --check
```

Default tests never open windows or inject input. See the [testing guide](docs/testing.md) for hardware tests and log analysis.

## Documentation

- [GUI guide](docs/gui.md)
- [Architecture](docs/architecture.md)
- [Testing](docs/testing.md)
- [Background capture & gesture suppression research](docs/background-gesture-feasibility.md)
- [Design spec (Chinese)](docs/Pad2Touch.md)

## Acknowledgments

Huge thanks to **[JohnAZoidberg/tapview](https://github.com/JohnAZoidberg/tapview)**, the touchpad visualizer whose Windows Raw Input / HID approach showed the way and inspired this project. Pad2Touch is an independent implementation — no Tapview code is included.

Thanks also to the [windows-rs](https://github.com/microsoft/windows-rs) project. See [third-party notices](docs/third-party-notices.md) for details.

## License

[MIT](LICENSE). Pad2Touch has no telemetry or network features. The mapping diagram's desktop thumbnail is only drawn on screen, never saved. Logs written to `artifacts/` may contain device paths — review them before sharing.
