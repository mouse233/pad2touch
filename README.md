# PadBridge

**Turn a Windows Precision Touchpad into a touchscreen.**

PadBridge reads absolute finger positions from your touchpad through Windows Raw Input and replays them as native multitouch input (`InjectTouchInput`) on a window or region of your screen. It runs entirely in user mode — no driver, no hooks into the target application.

> **Status:** experimental. Expect rough edges; settings are not saved between launches yet.

## Features

- Up to 5 simultaneous contacts, delivered as real `PT_TOUCH` events (not mouse emulation)
- Map the touchpad to the whole screen, a monitor, a dragged portion of the screen, or an application window (optionally just part of it) — window mappings follow the window as it moves or resizes
- Touchpad area modes for when shapes differ: keep the pad's shape, stretch it, or use part of the pad to fill the area
- A Wacom-style diagram shows your screens, the touchpad and the mapped region
- Native GUI with device picker, Start/Stop, and global Toggle / Hold shortcuts
- Built-in diagnostic view with per-finger trails and device info (size, DPI, report rate)
- Optional suppression of Windows' three-/four-finger touchpad gestures while capturing

## Requirements

- Windows x64 with a Precision Touchpad
- Rust (stable, MSVC toolchain) with Visual C++ build tools and a Windows SDK

## Getting started

```powershell
cargo build --release --locked
.\target\release\padbridge-phase0.exe
```

The GUI opens idle. Pick a touchpad, a **Screen area** and a **Touchpad** mode, then click **Start capture** (or enable a shortcut, Ctrl+Alt+F8 – F11). Touch the pad and lift all fingers once to arm injection.

**Ctrl+Alt+Esc** stops capture from anywhere.

Command-line modes (probe, raw capture, self-test, …) are available through the helper script:

```powershell
.\scripts\Run-PadBridge.ps1 -Mode probe
.\scripts\Run-PadBridge.ps1 -Mode capture -Inject -Contacts 5 -Seconds 120
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
- [Design spec (Chinese)](docs/PadBridge.md)

## Acknowledgments

Huge thanks to **[JohnAZoidberg/tapview](https://github.com/JohnAZoidberg/tapview)**, the touchpad visualizer whose Windows Raw Input / HID approach showed the way and inspired this project. PadBridge is an independent implementation — no Tapview code is included.

Thanks also to the [windows-rs](https://github.com/microsoft/windows-rs) project. See [third-party notices](docs/third-party-notices.md) for details.

## License

[MIT](LICENSE). PadBridge has no telemetry or network features. The mapping diagram's desktop thumbnail is only drawn on screen, never saved. Logs written to `artifacts/` may contain device paths — review them before sharing.
