# Native GUI

Version 0.4.0 adds Phase 3 window and rectangle mapping to the native GUI. Application-window, window-rectangle and screen-rectangle targets are implemented and covered by automated synthetic tests; physical-touchpad acceptance against third-party applications remains a manual step. This does not mark full Phase 2 hardware acceptance or all of Phase 4 complete.

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

With the Test area target, moving, resizing, minimizing, or changing the display stops active touch injection; restore/place the GUI before starting. Application targets follow their own window instead (see below), and the PadBridge window may then be moved or minimized freely. A display configuration change stops every injecting session. Raw-only sessions can remain active while minimized or behind another application. Settings are not persisted across launches; Always-on, tray management, and startup recovery are not implemented.

## Diagnostics and system gestures

The embedded receiver retains colored trails, two-contact distance scaling, parser/injection counters, lifecycle errors, gesture-controller state, and stop reasons. Controls sit outside the injectable rectangle. The interface uses system-drawn Win32 group boxes, combo boxes, check boxes, and buttons with the Windows theme and system dialog background. A back buffer presents completed diagnostic frames; native status text changes only when its value changes. Controls and the diagnostic header scale with DPI.

Each session writes `events.log` and `summary.txt` into a fresh `artifacts/gui-*` directory in the launch working directory. The GUI shows the location. Use `scripts\Analyze-Run.ps1` to audit native-injection runs afterward. Machine-specific logs remain local.

“Capture” or “takeover” does not promise exclusive device ownership. Native mouse movement, clicks, and scrolling remain active. The Windows system gestures selector offers Keep system gestures, Foreground guard, and Background guard (experimental). Foreground guard uses the existing foreground-only controller; switching to another application makes it ineligible. Background guard explicitly applies the validated settings-cache transaction described below. Injected touchscreen gestures remain a separate compatibility boundary.

## Background guard

Background guard temporarily changes four current-user three-/four-finger gesture preferences and refreshes the settings cache. It is an explicit option, off by default, and currently accepts only the validated Windows build 26300.9974. Each activation is bounded to 120 seconds. Expiry stops capture and disarms activation; start again explicitly for another interval.

The existing transaction script and independently implemented native cache helper are embedded in the executable. The build compiles that helper; runtime extracts assets and durable recovery journals under the session's local output directory. There is no need to install a separate helper or change PowerShell execution policy. The installed PowerShell runtime must permit local scripts.

The input worker starts only after the settings snapshot, independent watchdog, four writes, cache notifications, and readback succeed. On stop, it releases touch contacts before requesting restoration. GUI close waits for verified restoration and log finalization. Unverified recovery blocks further sessions. The script tracks the GUI's process identity; its separate watchdog also covers failure of the transaction process. Recovery assets are retained on disk, with the existing per-user lease excluding overlapping transactions.

The option changes user-wide settings during its interval; native pointer movement remains active. The adapter's apply/restore and a complete raw-only GUI session have been checked separately from physical gesture behavior. Simultaneous physical native-injection acceptance and forced-termination recovery remain separate tests. There is no startup recovery service and no guarantee against machine shutdown or killing all recovery processes.

## Mapping targets (Phase 3)

The **Screen area** group selects where touches go. It is kept short, like a pen-tablet panel: whole screen, portion of screen, a window, and a test area. The whole screen is the default.

| Choice | Configure | Bounds | Hit test |
|---|---|---|---|
| Screen · W×H (one monitor) | Nothing; **Show area** flashes it | That monitor | None (whatever is on screen receives it) |
| Full desktop (several monitors) | Nothing | The whole virtual desktop | None |
| Monitor N · W×H (several monitors) | Nothing; one entry per display, main display first | That monitor | None |
| Portion of screen… | **Select area…**, then drag on the dimmed desktop | Fixed physical virtual-desktop rectangle | None |
| Application window | Choose a window from the list | Whole client area (title bar and borders excluded) | Target window or its child windows |
| Application window, part | **Select part…**, drag inside its client area; **Whole window** resets | Normalized client-relative rectangle | Target window or its child windows |
| Test area (inside PadBridge) | Nothing | Embedded receiver, fixed per session | Exactly the receiver |

With a single monitor, the whole desktop and that monitor are the same area, so only one "Screen" entry is listed. The list is rebuilt when the display configuration changes. A selected monitor is matched by its display device name; if it disappears, the choice falls back to the first entry. Choosing another window resets a selected part. Full desktop requires a rectangular arrangement: if monitors of different sizes leave uncovered corners, it is rejected with an explanation (use a monitor or a portion instead).

### Mapping diagram

For every choice except the test area, the right panel shows a mapping diagram below the diagnostic text, similar to pen-tablet control panels:

- **Top:** each monitor drawn in its real arrangement and proportions, with a bezel, a stand under monitors on the bottom row, and a desktop thumbnail.
- **Bottom:** the touchpad at its declared physical aspect. If the device geometry is unavailable, a 1.6 placeholder is shown and labeled as such. In fill-area mode the unused part of the touchpad is darker.
- **Red:** the mapped region on the screens (the bounds a session would use), the used part of the touchpad, and four connectors between their corners. In keep-shape mode a thin gray frame shows the selected area, so its unreachable margins are visible.
- **During capture:** each live finger is drawn as a colored dot on the touchpad and, when it is inside the used part, at its mapped position on the screen.

While idle, the region is re-resolved every 250 ms, so moving a target window updates the diagram. During a session the diagram shows the session's current bounds, including retargets. If the target is not configured or not available, the diagram shows the reason instead of a region. The thumbnail is drawn only in this panel; it is never logged or written to disk.

The thumbnail is captured on demand rather than continuously: when the screen area or window selection changes, after an area selection, when the display configuration changes, and when PadBridge is activated again. Reading the composed desktop back from Windows takes about 33 ms (about 7 ms of CPU) on the GUI thread, so periodic capture would make the window hitch; ordinary repaints reuse the existing image at about 0.07 ms per repaint. No capture happens during a session. The image can therefore be slightly out of date, and switching back to PadBridge refreshes it.

The window list contains visible, uncloaked, unowned top-level windows of other processes, refreshed whenever the list opens. Titles are shown for selection only; logs record the handle, mode and physical bounds, never window titles. PadBridge's own windows cannot be application targets. Selections are not persisted across launches.

### Touchpad area

The **Touchpad** list decides what happens when the touchpad's physical shape differs from the screen area's shape:

| Choice | Touchpad used | Screen area used | Shape | Needs physical geometry |
|---|---|---|---|---|
| Whole pad · keep shape (default) | All | A centered region with the pad's shape | Undistorted | Yes |
| Whole pad · stretch | All | All | Scaled independently on X and Y | No |
| Part of pad · fill area | A centered part with the area's shape | All | Undistorted | Yes |

In fill-area mode, contacts outside the used part are ignored the same way as low-confidence contacts: they are not injected, a finger sliding out is released, and they still count as physically down, so arming still needs all fingers lifted. When a window target is resized, the used part is recomputed for its new shape together with the retarget. Stretch is the only mode that can inject when the device declares no usable physical size; the others refuse with an explanation. The test area honors the same setting.

The used touchpad part is logged with each `TARGET`, `TARGET_UPDATE` and `TARGET_RESUME` record (`pad_fit=`, `pad_input=`) and shown in the diagnostics target line.

### Selection

Selection uses a topmost dimmed overlay in physical pixels; Esc, right-click, the emergency stop or Stop cancels it and keeps the previous selection. Screen rectangles must be at least 16×16 pixels, lie inside the virtual desktop and have their corners and center on a display; negative secondary-monitor coordinates are supported. Window rectangles are stored as fractions of the client area, so they scale with the window. After a selection, a click-through outline drawn just outside the mapped region shows it for 1.5 seconds; the outline is never shown during a session. The Touchpad setting below decides how the touchpad's shape fits the area.

During a session the worker re-resolves window targets every 16 ms:

- Moved or resized: active contacts are cancelled at their last positions, the new bounds apply, and a new all-fingers-up frame is required (`TARGET_UPDATE`).
- Minimized, hidden or cloaked (another virtual desktop): contacts are released and frames are ignored until the window returns (`TARGET_PAUSE` / `TARGET_RESUME`).
- Closed: the session stops (`TARGET_LOST`) and activation is disarmed.

Because Windows routes injected touch by screen position, every non-cleanup DOWN/UPDATE point is hit-tested first; a point covered by another window cancels the sequence (`INJECT_BLOCKED`) instead of touching the wrong application. The diagnostics line also samples a 3×3 grid every 250 ms and reports `COVERED at n/9 sample points` as an early warning. Starting or enabling a shortcut warns if the target runs at a higher integrity level, because UIPI may reject injected input there.

The Foreground guard only applies while PadBridge itself is foreground, so it is normally inactive while you work in a target application; Background guard is the relevant option there.

`mapping::MappingTarget` carries the configuration; `ResolvedTarget` adds physical bounds, the hit policy and the physical aspect so the worker can re-resolve. `mapping::Tracker` is the pure state machine behind retarget/pause/resume/stop decisions.

`control::Controller` owns activation intent independently of native controls. `experiment::Session` owns worker lifecycle and logging. The GUI owns the persistent window and receiver dispatch, reusing the CLI's parser, injector, pointer handling, and diagnostic renderer.

## Window layout and repainting

The window has a fixed size: no sizing border and no maximize button (it can still be moved and minimized). The client area is 820 by 594 logical pixels, scaled using the monitor DPI and resized only when the DPI changes. The status field holds four lines (state plus three wrapped lines of detail). Non-client borders are calculated separately using AdjustWindowRectExForDpi. The settings column uses fixed compact rows. The diagnostics group, receiver, log field and stop hint are placed relative to the client bounds; controls are not vertically compressed.

Native labels, group boxes and read-only status fields use the system COLOR_BTNFACE brush through the control-color messages. Editable/selectable input surfaces retain their system theme.

The window avoids flicker by never showing a half-painted state. The main window uses WS_CLIPCHILDREN, so the dialog background is never erased under a control that then has to repaint itself. Group boxes, which do not paint their own interior, are opaque, sit at the bottom of the sibling Z-order and paint the background, frame and caption together through a back buffer; sibling clipping keeps them off the controls inside them. The receiver paints through its own back buffer. WS_EX_COMPOSITED was tried and rejected: under DWM it left controls blank while the window changed size.

Layout batches positions with DeferWindowPos and only moves windows whose bounds change, using Windows' normal invalidation. Unchanged controls are not repainted. Screen-coordinate changes refresh the displayed diagnostic rectangle; each running injection session retains its own immutable target and existing movement/resize stop behavior. The message font comes from system non-client metrics for the current DPI.

During a session the live contact preview repaints when input arrives rather than on the GUI's 30 ms timer, which limited it to about 32 frames per second at an uneven pace. After each touchpad frame the capture worker posts one message to the panel, which invalidates it. A flag keeps at most one such message queued, and WM_PAINT coalesces frames that arrive faster than the panel paints, so the preview follows the report rate up to what the panel can paint. Touches received by the diagnostic receiver also invalidate it directly. The timer still updates status and controls.

## Physical shape and device information

The diagnostic mapping rectangle uses the selected touchpad's declared physical X/Y dimensions, normalized with HID units and signed unit exponents. It fits centrally inside the available area with letterboxing and at most pixel-rounding error. Logical coordinate maxima alone are never used as a physical aspect ratio: X and Y coordinate densities can differ. Device selection, resize and movement refresh the preview; each injection session validates and retains the same physical aspect in its actual injection target. The physical-capture CLI receiver also preserves this aspect; synthetic API scenarios have no physical touchpad geometry.

The HID descriptor's logical ranges, physical lengths, counts/mm, equivalent coordinate DPI and field bit widths are available from --probe. The GUI shows physical size, width/height, logical X/Y ranges and DPI in its existing diagnostic output, including before starting capture. These are declared effective sensor dimensions, which can differ from outer glass or vendor-reserved areas. Field bit width describes report storage, not guaranteed measurement accuracy.

If a device omits valid physical dimensions/length units or declares conflicting coordinate geometry, the GUI shows it as unavailable and does not invent an aspect. Raw-only capture remains available; native injection is blocked until valid physical geometry is available.

Observed Hz distinguishes raw HID reports/s from assembled complete frames/s on the host over a recent active window. Hybrid report fragments must not be counted as separate touch frames. Scan cadence is independently estimated from complete active frames' 16-bit PTP Scan Time values (100 microseconds per tick), using median positive deltas with wraparound handling. Idle gaps, resets and insufficient samples are excluded; -- means unavailable rather than a claimed zero or nominal rate. This does not establish USB/I2C polling rate or end-to-end latency.
