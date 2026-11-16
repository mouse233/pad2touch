# PadBridge

**Map a Windows Precision Touchpad to native touchscreen input.**

PadBridge reads absolute finger coordinates through Windows Raw Input, maps them to a screen rectangle, and submits native touch frames through `InjectTouchInput`. It runs in user mode, without installing a driver or modifying another application.

The current implementation is an experimental single-touch and multitouch MVP. It includes a command-line runner and a small Win32 touch receiver. Mapping to arbitrary application windows, a full configuration UI, and production reliability work remain future stages.

## Features

- Discover HID touchpad collections with usage page `0x0D`, usage `0x05`.
- Parse Contact ID, Tip Switch, Confidence, and X/Y using Windows HID descriptor information.
- Assemble parallel and hybrid reports into complete contact frames.
- Map absolute touchpad coordinates to a fixed rectangle in physical screen pixels.
- Track a single finger or inject synchronized frames containing up to five contacts.
- Preserve contact identity across report reordering, partial lifts, and finger replacement.
- Commit contact state only after successful injection, retry `ERROR_NOT_READY`, and cancel contacts when capture stops or encounters an error.
- Display received `PT_TOUCH` contacts as separate colored trails, with a two-finger distance/scale demonstration.
- Check recorded event lifecycles and coordinates with an offline log verifier.

PadBridge produces touch events rather than mouse-click substitutes. Native touchpad pointer movement, clicks, scrolling, and system gestures remain active during capture.

## Build

Requirements:

- Windows x64 with the Raw Input and Touch Injection APIs.
- A recent stable Rust MSVC toolchain.
- Visual C++ build tools and a Windows SDK.
- A Precision Touchpad exposed as a Raw Input HID collection for physical-input capture.

```powershell
cargo build --release --locked
```

The executable currently retains the original package name: `target\release\padbridge-phase0.exe`.

## Run

Run from the repository directory in a normal PowerShell session. Basic operation is intended to work without administrator privileges.

```powershell
# Discover available touchpad collections.
.\scripts\Run-PadBridge.ps1 -Mode probe

# Capture raw input without injecting touch.
.\scripts\Run-PadBridge.ps1 -Mode capture -Contacts 5 -Seconds 120

# Single-contact mode.
.\scripts\Run-PadBridge.ps1 -Mode capture -Inject -Contacts 1 -Seconds 120

# Synchronized multitouch mode.
.\scripts\Run-PadBridge.ps1 -Mode capture -Inject -Contacts 5 -Seconds 180
```

Use `-Device N` when more than one collection is available. The contact capacity can be set from 1 to 5; actual input remains limited by the device.

At startup, touch the pad and lift all fingers once. Injection waits for a complete clear frame before entering `ARMED`. Keep the receiver window visible and stationary. Press Esc, close the window, use Ctrl+C, or wait for the timer to request cancellation and shutdown.

Each run writes to a new timestamped directory under `artifacts/`. An explicit `--out` directory containing an existing `events.log` is rejected to avoid overwriting an earlier run.

## Development and testing

```powershell
cargo test --locked
cargo clippy --all-targets -- -D warnings
cargo fmt -- --check
```

Default tests do not open windows or inject input. Two optional recording tests require local HID fixtures and are ignored by default; see [fixture documentation](tests/fixtures/README.md).

An optional synthetic API exercise submits **real native touch input** to the receiver window. Run it only when ready to interact with the desktop:

```powershell
.\scripts\Run-PadBridge.ps1 -Mode self-test -Contacts 5 -Seconds 40
```

To audit an existing run without injecting input:

```powershell
.\scripts\Analyze-Run.ps1 -RunDirectory artifacts\YOUR_RUN
```

See the [testing guide](docs/testing.md) for procedures and interpretation. Machine-specific test results are kept locally and are not part of the repository.

## Current limitations

- Injection targets the receiver's fixed rectangle. Arbitrary target windows, dynamic rectangle tracking, and a full GUI are not implemented.
- Raw Input does not provide exclusive touchpad capture. Native mouse behavior and system gestures can interfere with testing.
- Moving, minimizing, or covering the receiver may pause or cancel output. Display changes and device removal stop the experiment rather than automatically reconnecting.
- A gap of more than 500 ms in raw reports cancels active injection. Devices that stop reporting stationary contacts require particular attention during hold tests.
- Unsupported HID layouts are rejected. Windows preparsed data is opaque and is not a portable HID report descriptor.
- Contact cancellation can be rejected by Windows. Unresolved contacts remain tracked and block new DOWN events; forced termination and crashes cannot guarantee cleanup.
- The two-finger demonstration uses coordinates received by the test window. It does not establish gesture compatibility with every application.

## Documentation

- [Technical whitepaper and development specification (Chinese)](docs/PadBridge.md)
- [Architecture and implementation decisions](docs/architecture.md)
- [Testing guide](docs/testing.md)
- [Third-party notices](docs/third-party-notices.md)

## License and privacy

PadBridge's original code and accompanying documentation are licensed under the [MIT License](LICENSE). Dependencies retain their own licenses.

The program has no telemetry or network-upload feature and does not record ordinary keyboard input. Raw reports, logs, screenshots, local test reports, private HID recordings, build artifacts, credentials, and machine configuration are excluded by `.gitignore`. Review diagnostics before sharing them: device instance paths and screen content can be machine-specific.
