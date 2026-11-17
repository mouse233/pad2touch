# Native GUI

Version 0.3.2 integrates the diagnostic receiver into a persistent native Windows application. This advances the core capture-management GUI work from Phase 4 before arbitrary-window mapping in Phase 3. It does not mark full Phase 2 hardware acceptance, Phase 3, or all of Phase 4 complete.

## Launch and devices

Run `target\release\padbridge-phase0.exe --gui`, run the executable without arguments, or use `scripts\Run-PadBridge.ps1 -Mode gui`. The wrapper defaults to GUI mode. The GUI detaches its console; CLI capture, probe, self-test, and offline verification retain their console behavior.

The device list shows Precision Touchpad Raw Input collections with VID/PID. Refresh preserves the selected device path when present. Starting rediscovers and matches that path, so reordered indexes cannot silently change the selected source. Configuration is locked during capture and while shortcut activation is enabled. Stop/disarm first to change it. A disconnected or unsupported device produces a visible error.

The application opens idle. Native injection is selected by default, but capture only starts after an explicit action. Clearing **Inject native touch input** selects raw-only capture; no received touch trails are expected then. Contact capacity ranges from one to five, limited by the actual device.

## Capture controls

| Mode | Enable/start | Stop behavior |
|---|---|---|
| Manual start / stop | Click **Start capture**. | **Stop / disarm** stops the session. |
| Toggle with shortcut | Click **Enable shortcut**, then press the selected global chord. | Press again to stop; the chord remains enabled for another session. |
| Hold shortcut to capture | Click **Enable shortcut**, then hold the selected chord. | Releasing any chord key requests stop. A new press can start a subsequent session. |

Chords are Ctrl+Alt+F8, F9, F10, or F11. Registration uses `RegisterHotKey` with `MOD_NOREPEAT`; an occupied or rejected chord fails visibly and does not arm capture. Hold release samples only the configured chord's current states at approximately 30 ms intervals. It does not record arbitrary keyboard input. A late notification for a released chord cannot start capture. This timing is not a real-time guarantee.

**Stop / disarm**, Esc while the GUI has focus, and global Ctrl+Alt+Esc all disarm activation and request cleanup. A fresh enable action is needed afterward. The emergency chord's registration result remains visible; if unavailable, use the button or focused-window Esc.

Stop waits for the worker, then processes receiver messages for at least 300 ms before finalizing logs. Another session waits for that drain. Diagnostics and trails remain visible after stopping. Unresolved injected or received contacts prevent another session. Worker failures and safety stops disarm activation instead of automatically restarting capture.

Moving, resizing, minimizing, or changing the display stops active touch injection. Raw-only sessions can remain active while minimized or behind another application. Restore/place the GUI before starting injection. Mapping stays fixed during each session. Settings are not persisted across launches; Always-on, tray management, and startup recovery are not implemented.

## Diagnostics and system gestures

The embedded receiver retains colored trails, two-contact distance scaling, parser/injection counters, lifecycle errors, gesture-controller state, and stop reasons. Controls sit outside the injectable rectangle. The interface uses system-drawn Win32 group boxes, combo boxes, check boxes, and buttons with the Windows theme and system dialog background. A back buffer presents completed diagnostic frames; native status text changes only when its value changes. Controls and the diagnostic header scale with DPI.

Each session writes `events.log` and `summary.txt` into a fresh `artifacts/gui-*` directory in the launch working directory. The GUI shows the location. Use `scripts\Analyze-Run.ps1` to audit native-injection runs afterward. Machine-specific logs remain local.

“Capture” or “takeover” does not promise exclusive device ownership. Native mouse movement, clicks, and scrolling remain active. The Windows system gestures selector offers Keep system gestures, Foreground guard, and Background guard (experimental). Foreground guard uses the existing foreground-only controller; switching to another application makes it ineligible. Background guard explicitly applies the validated settings-cache transaction described below. Injected touchscreen gestures remain a separate compatibility boundary.

## Background guard

Background guard temporarily changes four current-user three-/four-finger gesture preferences and refreshes the settings cache. It is an explicit option, off by default, and currently accepts only the validated Windows build 26300.9974. Each activation is bounded to 120 seconds. Expiry stops capture and disarms activation; start again explicitly for another interval.

The existing transaction script and independently implemented native cache helper are embedded in the executable. The build compiles that helper; runtime extracts assets and durable recovery journals under the session's local output directory. There is no need to install a separate helper or change PowerShell execution policy. The installed PowerShell runtime must permit local scripts.

The input worker starts only after the settings snapshot, independent watchdog, four writes, cache notifications, and readback succeed. On stop, it releases touch contacts before requesting restoration. GUI close waits for verified restoration and log finalization. Unverified recovery blocks further sessions. The script tracks the GUI's process identity; its separate watchdog also covers failure of the transaction process. Recovery assets are retained on disk, with the existing per-user lease excluding overlapping transactions.

The option changes user-wide settings during its interval; native pointer movement remains active. The adapter's apply/restore and a complete raw-only GUI session have been checked separately from physical gesture behavior. Simultaneous physical native-injection acceptance and forced-termination recovery remain separate tests. There is no startup recovery service and no guarantee against machine shutdown or killing all recovery processes.

## Mapping extension points

`mapping::MappingTarget` separates target configuration from capture intent and native controls:

- `Diagnostics`: resolves the embedded receiver's physical-pixel rectangle for a new session.
- `ScreenRectangle { bounds }`: reserved screen-space bounds.
- `Window { handle }`: reserved target window identity.
- `WindowRectangle { handle, client_bounds }`: reserved client-relative bounds.

`ResolvedTarget` carries the receiver identity and physical bounds into `experiment::Session`. Reserved modes return explicit errors and cannot start from the GUI. They do not silently fall back to diagnostics. Phase 3 still needs target selection, window-lifetime validation, coordinate resolution, dynamic bounds updates, and updated hit-testing rules; the enum alone does not implement third-party injection.

`control::Controller` owns activation intent independently of native controls. `experiment::Session` owns worker lifecycle and logging. The GUI owns the persistent window and receiver dispatch, reusing the CLI's parser, injector, pointer handling, and diagnostic renderer.

## Window layout and repainting

The default client area is 820 by 480 logical pixels, scaled using the monitor DPI; minimum client size is 760 by 470. Non-client borders are calculated separately using AdjustWindowRectExForDpi. The settings column uses fixed compact rows. The diagnostics group, receiver, log field and stop hint stretch or anchor to the client bounds; controls are not vertically compressed.

Native labels, group boxes and read-only status fields use the system COLOR_BTNFACE brush through the control-color messages. Editable/selectable input surfaces retain their system theme. Transparent group boxes and sibling clipping preserve overlapping control ordering. The parent paints its background under group boxes, with no blanket WS_CLIPCHILDREN exclusion.

Layout batches positions with DeferWindowPos, disables copying stale client pixels, and queues invalidation/erasure for the parent and all children. Screen-coordinate changes refresh the displayed diagnostic rectangle; each running injection session retains its own immutable target and existing movement/resize stop behavior. The message font comes from system non-client metrics for the current DPI.

## Physical shape and device information

The diagnostic mapping rectangle uses the selected touchpad's declared physical X/Y dimensions, normalized with HID units and signed unit exponents. It fits centrally inside the available area with letterboxing and at most pixel-rounding error. Logical coordinate maxima alone are never used as a physical aspect ratio: X and Y coordinate densities can differ. Device selection, resize and movement refresh the preview; each injection session validates and retains the same physical aspect in its actual injection target. The physical-capture CLI receiver also preserves this aspect; synthetic API scenarios have no physical touchpad geometry.

The HID descriptor's logical ranges, physical lengths, counts/mm, equivalent coordinate DPI and field bit widths are available from --probe. The GUI shows physical size, width/height, logical X/Y ranges and DPI in its existing diagnostic output, including before starting capture. These are declared effective sensor dimensions, which can differ from outer glass or vendor-reserved areas. Field bit width describes report storage, not guaranteed measurement accuracy.

If a device omits valid physical dimensions/length units or declares conflicting coordinate geometry, the GUI shows it as unavailable and does not invent an aspect. Raw-only capture remains available; native injection is blocked until valid physical geometry is available.

Observed Hz distinguishes raw HID reports/s from assembled complete frames/s on the host over a recent active window. Hybrid report fragments must not be counted as separate touch frames. Scan cadence is independently estimated from complete active frames' 16-bit PTP Scan Time values (100 microseconds per tick), using median positive deltas with wraparound handling. Idle gaps, resets and insufficient samples are excluded; -- means unavailable rather than a claimed zero or nominal rate. This does not establish USB/I2C polling rate or end-to-end latency.
