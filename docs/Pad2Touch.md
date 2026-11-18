# Pad2Touch Technical Whitepaper

**Implementation baseline** Pad2Touch 0.4.0  
**Updated** 2026-11-18  
**Build targets** Windows x64 and ARM64  
**Status** Experimental release. Core features are available, and full hardware and release acceptance is pending.

## 1. Purpose and scope

Pad2Touch maps a Windows Precision Touchpad to an absolute area of the desktop and delivers the resulting contacts as native Windows touch input. It takes inspiration from macOS Touch Alternatives. The intended experience is to tap, drag, or use multiple fingers in a touch-aware Windows application without a touchscreen or changes to that application.

The basic input path runs in user mode with ordinary user privileges. The design uses published Windows Raw Input, HID, and touch injection APIs. It excludes custom drivers, firmware or HID Feature Report changes, target-process hooks or DLL injection, executable or memory modification, and emulator-specific interfaces. Input is delivered as `PT_TOUCH` through the Windows touch injection API. Mouse emulation and manually posted `WM_POINTER` messages are outside the design.

The application is written in Rust using `windows-sys` and native Win32 controls. The current GUI starts idle, lets users choose a device and mapping, and supports up to five simultaneous contacts on capable devices. CI builds portable executables for Windows x64 and ARM64. Windows 10 and Windows 11 remain compatibility test targets. Architecture-specific CI builds and a physical test on one device cover different parts of that matrix. [README](../README.md) · [CI workflow](../.github/workflows/ci.yml)

## 2. Input pipeline

```text
Precision Touchpad HID reports
  -> Windows Raw Input on a worker thread
  -> Report decoding and complete-frame assembly
  -> Contact tracking and absolute coordinate mapping
  -> Windows touch injection
  -> Windows hit testing and delivery to a touch-aware application
```

### Device discovery and HID parsing

Raw Input discovery selects the Digitizers / Touch Pad top-level collection (Usage Page `0x0D`, Usage `0x05`). Pad2Touch enumerates devices with `GetRawInputDeviceList`, queries metadata and preparsed data with `GetRawInputDeviceInfo`, and uses the Windows `HidP_*` APIs to interpret each device's layout. The GUI lets the user select a device and matches that selection again when capture starts.

A worker message window receives `WM_INPUT`. Registration with `RIDEV_INPUTSINK` allows background receipt, and `RIDEV_DEVNOTIFY` supplies device notifications. The worker checks the source device and processes every report in the `RAWHID` buffer using `dwCount` and `dwSizeHid`.

The decoder uses report IDs, link collections, field capabilities, and device-specific logical coordinate ranges. It reads Contact ID, Tip Switch, Confidence, X/Y, Contact Count, and Scan Time when available. A hybrid frame is assembled across reports using Contact Count and matching Scan Time. Malformed or unsupported layouts are rejected. Low-confidence contacts are excluded from injection. Report interpretation and frame assembly can be tested separately from the Windows APIs that supply HID data. [Input discovery and frame assembly](architecture.md#input-discovery-and-parsing)

### Contact tracking and injection

Each injected contact keeps a stable slot for its lifetime regardless of report order. A frame carries all active contacts and their changes together. When a finger lifts, the remaining contacts continue with their existing slots. A slot becomes available after its release frame succeeds. If departures and arrivals exceed the available slots in one submission, the planner releases departing contacts before introducing new ones.

Each injection session calls `InitializeTouchInjection` with its capacity, then submits a `POINTER_TOUCH_INFO` array to `InjectTouchInput` for each planned frame. The GUI uses a capacity of five, and CLI diagnostics accept one through five. The input worker processes reports and injection in order, independently of GUI painting and diagnostic log writing.

| Contact action | Flags sent by the current transport |
| --- | --- |
| Start | `INRANGE + INCONTACT + DOWN` |
| Move or stationary hold | `INRANGE + INCONTACT + UPDATE` |
| End | `UP` |
| Cancel | `CANCELED + UP` |

A release uses the last successfully injected position. The planner commits a frame's state after the API accepts it, retries `ERROR_NOT_READY` without advancing state, and handles `ERROR_INVALID_PARAMETER` as cancellation. Other failures trigger cleanup. Pending cleanup blocks new contacts. Stationary contacts receive keepalive updates. Pressure, orientation, and contact area are left unspecified. [Native injection and recovery](architecture.md#native-injection-and-recovery)

## 3. Absolute mapping and delivery

Pad2Touch normalizes each axis using the device's HID logical range. For a selected touchpad portion, it renormalizes coordinates within that portion before projecting them into a physical-pixel rectangle. The projection uses the inclusive last pixel, then rounds and clamps the result.

```text
normalized_x = (raw_x - min_x) / (max_x - min_x)
screen_x = left + normalized_x * (width - 1)
```

The same calculation applies to Y with the target's top and height. The process and input worker use Per-Monitor DPI Awareness V2. Monitor coordinates may be negative in the virtual desktop.

| Target | Mapping behavior |
| --- | --- |
| Application window | Map to the client area, excluding the caption and frame. Follow the window when it moves or resizes. |
| Window rectangle | Store the selected rectangle as fractions of the client area so it moves and scales with the window. |
| Desktop rectangle | Choose a screen area by dragging on an overlay. The GUI also offers a monitor or the full desktop when the monitor layout forms a rectangle. |
| Test area | Map to the embedded receiver to inspect touch trails and the two-finger scaling preview. |

The GUI offers **Force proportions**, **Full**, and a user-selected **Portion** for the touchpad. Contacts outside the active portion are excluded from injection. Optional axis inversion, rotation, mapping margins, calibration, and smoothing remain planned. The design favors unsmoothed input by default to limit latency. [Mapping controls](gui.md#touchpad-area)

`InjectTouchInput` submits coordinates to the interactive desktop. Windows chooses the recipient by hit testing. Pad2Touch checks that a point in a window-target session belongs to that window or a child before sending a normal frame. An obscured point blocks injection. When a target becomes hidden or minimized, the session pauses and releases active contacts. It can resume once the target is usable. Destroying the target stops the session. Retargeting ends the old contact lifetimes before applying new bounds. Elevated applications may reject input. [Window and rectangle targets](architecture.md#window-and-rectangle-targets)

## 4. Capture controls and diagnostics

| Control | Current behavior |
| --- | --- |
| Manual | Start and stop from the main window. |
| Toggle | Enable a preset global shortcut, then press it to start or stop. The choices are Ctrl+Alt+F8 through Ctrl+Alt+F11. |
| Hold | Enable the shortcut, then hold Alt to capture. Release Alt to stop. |
| Emergency stop | Ctrl+Alt+Esc stops capture and disarms the shortcut when global registration succeeds. The Stop button and focused-window Esc provide local controls. |

Capture starts after explicit activation. A valid first contact frame can begin injection immediately. After recovery or retargeting, the planner waits for all physical tips to lift before starting fresh contact lifetimes. The GUI waits for cleanup and receiver drain before allowing another session. A persistent configuration, a dedicated Always-on mode, arbitrary shortcut bindings, and tray controls remain planned. [Capture controls](gui.md#capture-controls)

The Test area displays independent contact trails and a two-finger scale preview. Launching with `--diagnostics` adds device geometry, coordinate ranges, report rates, capture counters, and gesture state, and saves a session log and summary locally. The CLI also provides device probing, raw-only capture, a synthetic native API exercise, and offline log verification. [Diagnostics](gui.md#diagnostics-and-system-gestures)

## 5. System input, recovery, and privacy

Windows can continue turning physical touchpad activity into pointer movement, clicks, and scrolling while Pad2Touch receives the Raw Input stream. Exclusive touchpad capture is outside the current design. Any future pointer suppression would need reliable device-level isolation so an external mouse and other applications keep working.

The GUI offers two independent gesture overrides. **Block touchpad gestures** temporarily changes current-user three- and four-finger touchpad preferences. **Block touchscreen gestures** changes the separate live touchscreen gesture state, including actions triggered by injected touch. Both checkboxes start selected in 0.4.0. The implementation currently restricts these overrides to Windows build `26300.9974`. With both options cleared, capture leaves those gesture settings untouched. The original release criterion was for normal capture to leave touchpad preferences untouched. The current defaults temporarily change them on the validated build, so the defaults and that criterion still need to be reconciled before release acceptance. [Gesture options and compatibility](gui.md#diagnostics-and-system-gestures)

The gesture worker records a recovery snapshot and verifies the requested override before capture. On normal stop it restores the snapshot. An independent watchdog can attempt restoration after loss of the GUI or backend. Machine shutdown or termination of all recovery processes can bypass that path. Automatic capture reconnection and a startup recovery service remain outside 0.4.0. [Gesture restoration](gui.md#background-guard)

Touch cleanup is a release requirement whenever capture stops, including manual and emergency stop, exit, target loss, device removal, a display change, or an unrecoverable injection error. A temporarily unavailable window releases contacts before pausing. Failed or unverified cleanup must be visible and block new contact lifetimes. Forced termination, secure-desktop transitions, and failed Windows API calls can prevent cooperative cleanup. [Receiver and shutdown](architecture.md#receiver-and-shutdown)

Pad2Touch has no telemetry or remote input transport. It uses key state for capture shortcuts and does not record ordinary keyboard input. The desktop thumbnail is drawn on screen and is not written to disk during normal use. Diagnostic logs, screenshots from explicit tests, and private HID recordings can contain device or screen information. Keep them in ignored local directories and review them before sharing. [Documentation and privacy](architecture.md#documentation-and-privacy) · [Private recording fixtures](../tests/fixtures/README.md)

## 6. Development status

The evidence below describes specific runs or test coverage. A passing unit test, an accepted injection call, a verified receiver log, and a completed hardware acceptance run each establish a different result.

| Phase | Implemented | Verified | Pending acceptance or implementation |
| --- | --- | --- | --- |
| 0. Feasibility | Receive physical Precision Touchpad reports and inject a single contact into a fixed rectangle. | On one device, the receiver recorded real `PT_TOUCH` Down, Update, and Up from physical input. | Phase 0 demonstrated feasibility on that setup. Wider hardware coverage belongs to release acceptance. |
| 1. Single-contact MVP | Device selection, stable single-finger tracking, absolute mapping, injection, and release. | A first physical single-contact run passed log review, with matched Down/Up and no contacts remaining. | Repeat the 30-second hold and outstanding edge cases on the current build. |
| 2. Multitouch | Complete frames, independent contacts, synchronized injection of up to five contacts, a two-finger preview, and error recovery. | An existing physical GUI capture passed offline verification with a receiver peak of five contacts, 46 matched lifetimes, and no residual contacts. | Complete the physical gesture and cleanup checklist, including sustained and repeated multi-contact runs and coverage across devices. |
| 3. Window and rectangle mapping | Window, window rectangle and desktop rectangle targets, dynamic retargeting, DPI-aware coordinates, and hit testing. | Automated synthetic coverage for coordinate mapping and target changes. | Complete physical-touchpad testing against third-party applications at 125%, 150%, and 200% display scaling. |
| 4. Capture management and GUI | Manual, Toggle and Hold, emergency stop, device selection, mapping preview, and diagnostics. | Default tests cover capture intent and coordinate logic. The testing guide gives separate interactive GUI checks. | Add persistent settings, dedicated Always-on, and arbitrary shortcut bindings. Complete the interactive activation and shutdown checks. |
| 5. Reliability and distribution | Safety stops for device removal and display changes, injection cleanup and recovery handling, and portable x64 and ARM64 CI builds. | CI runs default tests and uploads the portable executable for each architecture. The gesture-specific watchdog has native test evidence on the validated Windows build. | Add automatic device reconnection, capture interruption recovery, tray controls, and an installer. Finish compatibility, reliability, and performance acceptance. |

At this update, `cargo test --locked --offline` passed 91 tests with 17 ignored by design. The ignored set includes tests requiring native GUI interaction, gesture changes, injection, or private recordings. The physical five-contact observation comes from an existing local capture and was checked with the offline verifier. The repository keeps machine-specific observations and raw artifacts local. [Testing guide](testing.md) · [Offline log verification](architecture.md#offline-log-verification)

## 7. Performance targets and release acceptance

| Measure | Design target |
| --- | --- |
| Responsiveness | Keep UI painting and diagnostic log I/O off the time-critical input path. Submit touch frames in order without silently dropping Down or Up transitions. |
| Software processing latency | P95 under 10 ms. Report the measured interval, such as Raw Input dispatch to successful injection, along with the build, device, and workload. |
| Idle CPU | Below 1% where practical. |
| Active memory | Below 100 MB where practical. |
| Contacts and cleanup | Support at least five simultaneous contacts on capable hardware and leave no stuck contacts after a tested stop. |

The software timing interval excludes touchpad scan time and final application delivery. The P95, CPU, and memory targets still need a documented baseline for the current release. [Device and report-rate diagnostics](gui.md#physical-shape-and-device-information)

Complete the following checks before treating the whitepaper's release acceptance as closed.

| Area | Required evidence |
| --- | --- |
| Physical input | Test taps, rapid taps, long holds, dragging, simultaneous movement, pinch in and out, three- through five-finger input, partial release, and simultaneous release on hardware. Confirm actual reception and that contacts return to zero. |
| Coordinates and delivery | Check corners, center, different rectangle sizes, moving and resizing windows, occlusion, negative coordinates, multiple monitors, and 125%, 150%, and 200% DPI. Use third-party touch-aware applications as well as the built-in receiver. Test axis inversion when that control is available. |
| Stability and recovery | Exercise target switches and closure, device removal, resolution changes, lost shortcut release, emergency stop, application exit, injection failure, and sustained dragging. Verify cleanup and the documented restore or restart behavior. |
| Compatibility | Cover Windows 10 and Windows 11, multiple Precision Touchpad vendors and contact capacities, and both built-in and external pads where available. Record the Windows build and architecture for each result, including the gesture options tested. |
| Distribution and operating boundaries | Verify basic capture as an ordinary user, with native touch delivery and no custom driver or target-app changes. Resolve the gesture defaults, verify that ordinary capture preserves preferences, and verify exact restoration when overrides are enabled on the supported build. Document concurrent pointer behavior. Check the portable builds and define installer acceptance when the installer exists. |

Use reproducible procedures and record each case as passed, failed, or untested. Keep offline tests for decoding, frame assembly, mapping, contact planning, and recovery. Run interactive tests separately. Record important design decisions in the architecture notes and distinguish API failures, receiver behavior, and hardware limits in test reports. [Testing procedures](testing.md) · [Architecture notes](architecture.md)

## 8. References

- [Windows Precision Touchpad collection](https://learn.microsoft.com/en-us/windows-hardware/design/component-guidelines/touchpad-windows-precision-touchpad-collection)
- [Microsoft Precision Touchpad input guidance](https://learn.microsoft.com/en-us/windows/win32/input-precisiontouchpad/precision-touchpad-portal)
- [Microsoft Raw Input overview](https://learn.microsoft.com/en-us/windows/win32/inputdev/about-raw-input) and [WM_INPUT](https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-input)
- [InitializeTouchInjection](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-initializetouchinjection), [InjectTouchInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-injecttouchinput), and [POINTER_TOUCH_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-pointer_touch_info)
- [Tapview](https://github.com/JohnAZoidberg/tapview), a technical reference for Windows Precision Touchpad capture. Pad2Touch's licensing and attribution are documented in the [third-party notices](third-party-notices.md).
