# PadBridge

**Map a Windows Precision Touchpad to native touchscreen input.**

PadBridge reads absolute finger coordinates through Windows Raw Input, maps them onto a screen area, and submits native touch frames through `InjectTouchInput`. It turns a laptop touchpad into a small absolute touchscreen for a monitor, a window, or part of a window, much like a pen tablet's mapping. It runs in user mode: no driver, no DLL injection, no changes to the target application.

**Status:** experimental. Single-touch and synchronized multitouch injection, the native GUI, and Phase 3 window/rectangle mapping are implemented and covered by automated tests, including synthetic native injection into a real window. Acceptance with physical touchpads against third-party applications is still manual. Persistent settings, tray management and release packaging are future stages.

## Features

**Input**
- Discover HID touchpad collections (usage page `0x0D`, usage `0x05`) and select one from the GUI.
- Parse Contact ID, Tip Switch, Confidence and X/Y from the Windows HID descriptor; assemble parallel and hybrid reports into complete frames.
- Read the touchpad's declared physical size, logical ranges, coordinate density and observed report/frame/scan cadence.

**Mapping**
- **Screen area**, chosen like a pen tablet: the whole screen (or a specific monitor), a dragged portion of the screen, or an application window's client area, optionally narrowed to a dragged part of it. Physical pixels throughout, including negative multi-monitor coordinates and per-monitor DPI.
- **Touchpad area**, for when the touchpad's shape differs from the screen area: use the whole pad and keep its shape, stretch the whole pad, or use a centered part of the pad to fill the whole area.
- A **mapping diagram** shows your monitors, the touchpad at its physical shape, the mapped region in red, and live finger positions during capture.
- Window targets are followed live: moving or resizing retargets, minimizing or hiding pauses and releases contacts, closing stops. Touches where another window covers the target are blocked instead of reaching the wrong application.

**Touch injection**
- Track one finger or inject synchronized frames of up to five contacts with stable identities across reordering, partial lifts and replacement.
- Commit contact state only after successful injection, retry `ERROR_NOT_READY`, and always cancel contacts on stop, error, retarget or emergency stop.

**Capture control and diagnostics**
- Start/stop manually, toggle with a global shortcut, or capture while holding it; Ctrl+Alt+Esc is the emergency stop.
- An embedded receiver shows received `PT_TOUCH` trails, counters and a two-finger scale demonstration.
- An offline verifier audits recorded lifecycles, coordinates and target bounds.
- Optional three-/four-finger gesture guards (foreground controller, or an experimental bounded background settings override).

PadBridge produces touch events, not mouse-click substitutes. Native touchpad pointer movement, clicks and scrolling remain active. System gestures remain active unless a gesture guard is selected.

## Quick start

```powershell
cargo build --release --locked
.\target\release\padbridge-phase0.exe        # opens the GUI
```

1. **Input device:** pick your touchpad. The diagnostics panel shows its physical size.
2. **Screen area:** choose where touches go. The whole screen is the default.

   | Choice | What it maps to |
   |---|---|
   | Screen · W×H | The whole screen. With several monitors this becomes **Full desktop** plus one **Monitor N** entry per display |
   | Portion of screen… | **Select area…**, then drag on the dimmed desktop |
   | Application window | A window from the list (client area, without title bar). **Select part…** narrows it to a dragged part; **Whole window** goes back |
   | Test area (inside PadBridge) | PadBridge's own receiver, for checking touch delivery |

3. **Touchpad:** choose how shapes are reconciled.

   | Choice | Touchpad | Screen area | Shape |
   |---|---|---|---|
   | Whole pad · keep shape (default) | All of it | Centered region with the pad's shape; the rest is unreachable | Undistorted |
   | Whole pad · stretch | All of it | All of it | Distorted if shapes differ |
   | Part of pad · fill area | Centered part with the area's shape; touches outside are ignored | All of it | Undistorted |

   For example, a 1.6:1 touchpad mapped to a tall phone-emulator window either leaves most of the window unreachable (keep shape), squeezes movement horizontally (stretch), or uses only the middle strip of the pad (fill area).
4. The diagram shows the mapped region; **Show area** also flashes it on screen.
5. Click **Start capture**, or pick Toggle/Hold and **Enable shortcut** (Ctrl+Alt+F8–F11). Touch the pad and lift all fingers once to arm.
6. **Stop / disarm**, Esc in PadBridge, or Ctrl+Alt+Esc anywhere stops capture and releases all contacts.

Clear **Inject native touch input** to capture raw input only. Settings are not saved between launches. See the [GUI guide](docs/gui.md) for details.

## Build

Requirements:

- Windows x64 with the Raw Input and Touch Injection APIs.
- A recent stable Rust MSVC toolchain, Visual C++ build tools and a Windows SDK.
- A Precision Touchpad exposed as a Raw Input HID collection for physical capture.

```powershell
cargo build --release --locked
```

The executable keeps the original package name: `target\release\padbridge-phase0.exe`. Administrator privileges are not required.

## Command line

The CLI uses its own fixed receiver window. It is useful for focused hardware tests and scripted runs:

```powershell
.\scripts\Run-PadBridge.ps1 -Mode gui                     # same as running the executable
.\scripts\Run-PadBridge.ps1 -Mode probe                   # list touchpad collections and geometry
.\scripts\Run-PadBridge.ps1 -Mode capture -Contacts 5 -Seconds 120             # raw input only
.\scripts\Run-PadBridge.ps1 -Mode capture -Background -Contacts 5 -Seconds 75  # raw input while another app is foreground
.\scripts\Run-PadBridge.ps1 -Mode capture -Inject -Contacts 1 -Seconds 120     # single contact
.\scripts\Run-PadBridge.ps1 -Mode capture -Inject -Contacts 5 -Seconds 180     # multitouch
.\scripts\Run-PadBridge.ps1 -Mode capture -Inject -SuppressSystemGestures -Contacts 5 -Seconds 180
```

Use `-Device N` when several collections exist. Touch the pad and lift all fingers once; injection waits for a clear frame before `ARMED`. Keep the receiver visible and stationary. Esc, closing the window, Ctrl+C or the timer request cancellation and shutdown. Each run (GUI or CLI) writes `events.log` and `summary.txt` to a new directory under `artifacts/`.

### Experimental GUI background guard

**Background guard (experimental)** under **Windows system gestures** temporarily overrides the four three-/four-finger gesture preferences for up to 120 seconds per session, then restores them. Restoration is verified, and an independent watchdog attempts it if PadBridge exits unexpectedly. It is off by default and restricted to the validated Windows build 26300.9974. Because target applications are normally foreground during Phase 3 use, this is the relevant guard for them. See the [GUI guide](docs/gui.md).

### Foreground system-gesture ownership

`-SuppressSystemGestures` (GUI: **Foreground guard**) uses `Windows.UI.Input.TouchpadGesturesController` to claim three- and four-finger touchpad gestures while PadBridge itself is the foreground process. No registry values or preferences are changed. The API is experimental: check with `padbridge-phase0.exe --gesture-support` and `--gesture-check`. It does not suppress native mouse movement or Windows' separate touchscreen gestures; see [Microsoft's note on three-/four-finger touch](https://learn.microsoft.com/en-us/troubleshoot/windows/win32/finger3or4-touch-interaction-no-longer-works).

## Development and testing

```powershell
cargo test --locked
cargo clippy --all-targets -- -D warnings
cargo fmt -- --check
```

Default tests never open windows or inject input. They cover decoding, frame assembly, contact planning, mapping geometry, Touchpad area modes, the window tracker, the mapping diagram layout and the log verifier.

Opt-in native tests run against a helper window owned by a second process, which the test binary launches itself. Run them on an interactive desktop and leave input alone while they run:

```powershell
cargo test --locked native_external_window_targets_follow_move_resize_minimize_and_close -- --ignored --test-threads=1
cargo test --locked native_gui_phase3_target_selection_without_capture -- --ignored --test-threads=1
cargo test --locked native_gui_mapping_diagram_draws_region_and_touchpad -- --ignored --test-threads=1
# Submits REAL native touch input into the helper window:
cargo test --locked native_touch_injection_lands_in_external_window_at_mapped_positions -- --ignored --test-threads=1 --nocapture
```

To audit a recorded run without injecting input:

```powershell
.\scripts\Analyze-Run.ps1 -RunDirectory artifacts\YOUR_RUN
```

See the [testing guide](docs/testing.md) for every procedure, including the CLI self-test, physical-touchpad checks and DPI acceptance. Machine-specific results stay local.

## Current limitations

- `InjectTouchInput` has no window parameter: Windows delivers touches by screen hit testing. The target must stay visible. Covered points are blocked, but PadBridge cannot deliver into hidden parts of a window.
- Targets at a higher integrity level (for example, running as administrator) may reject injected input because of UIPI. The GUI warns but cannot bypass this.
- Raw Input does not give exclusive touchpad capture: the pointer still moves. Gesture guards have the scope limits described above.
- Display configuration changes and device removal stop capture rather than reconnecting automatically. Settings are not persisted.
- More than 500 ms without raw reports cancels active contacts. Devices that stop reporting stationary fingers need attention in hold tests.
- Unsupported HID layouts are rejected; Windows preparsed data is not a portable HID descriptor.
- Windows can reject contact cancellation. Unresolved contacts stay tracked and block new DOWN events; forced termination and crashes cannot guarantee cleanup.
- Synthetic native tests have run at 100% display scaling; 125%, 150% and 200% DPI acceptance and multi-monitor diagrams on real hardware still need manual checks.

## Documentation

- [Technical whitepaper and development specification (Chinese)](docs/PadBridge.md)
- [Architecture and implementation decisions](docs/architecture.md)
- [GUI guide: screen area, touchpad area, diagram, capture controls](docs/gui.md)
- [Testing guide](docs/testing.md)
- [Background capture and gesture-suppression feasibility](docs/background-gesture-feasibility.md)
- [Third-party notices](docs/third-party-notices.md)

## License and privacy

PadBridge's original code and documentation are licensed under the [MIT License](LICENSE). Dependencies retain their own licenses.

There is no telemetry or network upload, and ordinary keyboard input is never recorded. Window titles are shown only in the target list; logs record window handles and geometry. The desktop thumbnail in the mapping diagram is drawn on screen only, never logged or saved by the application. The opt-in diagram test does write `artifacts/diagram-*.bmp` previews that include it. Logs, screenshots, recordings and build artifacts are excluded by `.gitignore`. Review diagnostics before sharing them: device paths and screen content are machine-specific.
