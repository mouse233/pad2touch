# Native GUI

Version 0.4.0 adds Phase 3 window and rectangle mapping to the native GUI. Application-window, window-rectangle and screen-rectangle targets are implemented and covered by automated synthetic tests; physical-touchpad acceptance against third-party applications remains a manual step. This does not mark full Phase 2 hardware acceptance or all of Phase 4 complete.

## Launch and devices

Run `target\release\Pad2Touch.exe --gui`, run the executable without arguments, or use `scripts\Run-Pad2Touch.ps1 -Mode gui`. The wrapper defaults to GUI mode. The GUI detaches its console; CLI capture, probe, self-test, and offline verification retain their console behavior.

The device list shows Precision Touchpad Raw Input collections, numbered in discovery order. Diagnostics mode adds VID/PID. Refresh preserves the selected device path when present. Starting rediscovers and matches that path, so reordered indexes cannot silently change the selected source. Configuration is locked during capture and while shortcut activation is enabled. Stop/disarm first to change it. A disconnected or unsupported device produces a visible error.

The application opens idle. Native injection is selected by default, but capture only starts after an explicit action. Clearing **Inject native touch input** selects raw-only capture; no received touch trails are expected then. The GUI always initializes the maximum supported capacity of five contacts; devices with fewer contacts still work. Click Start capture and use the pad directly: no preliminary touch-and-lift is required. The first complete, confident contact frame can begin injection, including contacts already present when capture starts. Errors, cancellation, and mapping changes retain their all-fingers-up recovery gate.

**Show Windows touch feedback** (on by default, available only with injection) makes Windows draw its standard touch visuals at the injected contacts. It initializes injection with `TOUCH_FEEDBACK_DEFAULT` instead of `TOUCH_FEEDBACK_NONE`, so the system **Touch indicator** setting still decides whether the visuals appear. The mode is fixed when a session starts; change it between sessions.

## Capture controls

| Mode | Enable/start | Stop behavior |
|---|---|---|
| Manual start / stop | Click **Start capture**. | **Stop / disarm** stops the session. |
| Toggle with shortcut | Click **Enable shortcut**, then press the selected global chord. | Press again to stop; the chord remains enabled for another session. |
| Hold Alt to capture | Click **Enable shortcut**, then press and hold Alt. | Release Alt to stop. Press Alt again to start another session. |

Toggle uses Ctrl+Alt+F8, F9, F10, or F11. If Windows rejects the chosen chord, Pad2Touch shows an error before arming. Hold samples Windows' Alt key state at approximately 30 ms intervals and leaves the key available to Windows and other apps. If Alt is already down when you enable Hold, release it and press again.

**Stop / disarm**, Esc while the GUI has focus, and global Ctrl+Alt+Esc all disarm activation and request cleanup. A fresh enable action is needed afterward. The emergency chord's registration result remains visible; if unavailable, use the button or focused-window Esc.

Stop waits for the worker, then processes receiver messages for at least 300 ms. Another session waits for that drain. Test area trails remain visible after stopping. Unresolved injected or received contacts prevent another session. Worker failures and safety stops disarm activation instead of automatically restarting capture.

With the Test area target, moving, resizing, minimizing, or changing the display stops active touch injection; restore/place the GUI before starting. Application targets follow their own window instead (see below), and the Pad2Touch window may then be moved or minimized freely. A display configuration change stops every injecting session. Raw-only sessions can remain active while minimized or behind another application. Settings are not persisted across launches; Always-on, tray management, and startup recovery are not implemented.

## Diagnostics and system gestures

**Test area** shows colored trails, active touches, and a two-finger scaling preview inside Pad2Touch. Controls sit outside the injectable rectangle. Settings and the preview scale with DPI.

Launch `Pad2Touch.exe --diagnostics` or run `scripts\Run-Pad2Touch.ps1 -Diagnostics` to show device geometry, report rate, capture counters, pointer IDs, and gesture-controller details. This also writes `events.log` and `summary.txt` into a fresh `artifacts/runs/gui-*` directory per session and shows its path in the GUI. Use `scripts\Analyze-Run.ps1` to audit native-injection runs afterward. Normal GUI capture creates no `artifacts` directory. When a session uses a gesture override, the worker and watchdog keep their recovery files under `%LOCALAPPDATA%\Pad2Touch\recovery` in either mode.

“Capture” or “takeover” does not promise exclusive device ownership. Native mouse movement, clicks, and scrolling remain active. **Windows system gestures** contains two independent check boxes, both selected by default: **Block touchpad gestures** suppresses three-/four-finger touchpad swipes and taps, and **Block touchscreen gestures** suppresses Windows' separate three-/four-finger touchscreen system actions while retaining application multitouch. Either can be used alone, both can be used together, or both can be cleared. The selections apply equally to injecting and raw-only sessions and are locked during capture. Foreground-only gesture ownership remains available through the CLI diagnostic option.

## Background guard

The touchpad check box temporarily changes four current-user three-/four-finger gesture preferences and refreshes the settings cache. The touchscreen check box controls the separate live touchscreen state. These overrides currently accept only the validated Windows build 26300.9974. GUI sessions have no fixed duration: selected overrides remain active until capture stops. The standalone experiment's apply mode remains bounded for diagnostic use.

The diagnostic guard line displays the selected background guard's lifecycle independently of foreground controller ownership. During a session, the backend checks the four owned registry values and cached assignments once per second. Cache drift triggers a precise refresh only while all four registry values remain the session's disabled DWORDs. External preference changes, failed verification, or watchdog exit stop capture and trigger restoration; they are recorded in the session's `failed.json` and guard logs. These checks verify settings, not physical suppression or injected touchscreen gestures.

The executable provides both the native gesture worker and the independent recovery process. Manual and Toggle sessions start the worker on demand. Enabling Hold prepares a worker for reuse when a gesture override is selected. The worker stays idle until a capture request and snapshots the current settings for each session. Disarming releases the idle worker.

The input worker starts after the settings snapshot is saved, the independent watchdog is ready, and the requested overrides pass readback. On stop, it releases touch contacts before requesting restoration. GUI close waits for verified restoration and log finalization. Unverified recovery blocks further sessions. The watchdog tracks the GUI and worker identities and can restore the snapshot if either process exits. A stalled worker triggers recovery after 30 seconds without progress. The GUI also stops capture if the worker becomes unresponsive. The snapshot stays on disk, and the per-user lease excludes overlapping transactions.

When both check boxes are selected, the diagnostic line reads `ACTIVE (pad + screen)`; touchscreen-only reads `ACTIVE (screen)`. Applications still receive the injected multi-contact stream. Touchscreen-only sessions do not read, change, refresh, or restore touchpad preferences. The touchscreen override uses the build-bound, undocumented `SystemParametersInfoW` actions `0x2030` (read) and `0x2031` (write), with flags zero to avoid persisting `HKCU\Control Panel\Desktop\TouchGestureSetting`. The journal records both original live state and the exact presence/type/bytes of that preference. Normal stop and the independent watchdog restore the original live state and verify the persistent value was unchanged. External changes are retained and reported for recovery review.

The option changes user-wide settings during its interval; native pointer movement remains active. On build 26300.9974, native tests verified the combined live override and exact restoration, three-/four-/five-contact injected downward swipes reaching a separate helper window with balanced DOWN/UP events and no foreground loss or minimization, and independent watchdog restoration after terminating only the transaction backend. Physical touchpad acceptance across gesture directions remains a separate manual check. There is no startup recovery service and no guarantee against machine shutdown or killing all recovery processes.

The touchscreen SPI identifier was independently implemented from the first-person observation in [this Microsoft Q&A discussion](https://learn.microsoft.com/en-us/answers/questions/1530238/how-to-enable-disable-three-and-four-finger-touch); it is not a documented Windows compatibility contract. The older `0x009B` recipe in that discussion is not used. Microsoft documents the separation between system and application three-/four-finger touch interactions in [this troubleshooting guide](https://learn.microsoft.com/en-us/troubleshoot/windows/win32/finger3or4-touch-interaction-no-longer-works).

## Mapping targets (Phase 3)

In **Mapping**, **Screen area** selects where touches go and **Touchpad area** selects how much of the touchpad is mapped. The whole screen is the default target.

| Choice | Configure | Bounds | Hit test |
|---|---|---|---|
| Screen · W×H (one monitor) | Nothing; **Show area** flashes it | That monitor | None (whatever is on screen receives it) |
| Full desktop (several monitors) | Nothing | The whole virtual desktop | None |
| Monitor N · W×H (several monitors) | Nothing; one entry per display, main display first | That monitor | None |
| Portion of screen… | **Select area…**, then drag on the dimmed desktop | Fixed physical virtual-desktop rectangle | None |
| Application window | Choose a window from the list | Whole client area (title bar and borders excluded) | Target window or its child windows |
| Application window, part | **Select part…**, drag inside its client area; **Whole window** resets | Normalized client-relative rectangle | Target window or its child windows |
| Test area | Nothing | Embedded receiver, fixed per session | Exactly the receiver |

With a single monitor, the whole desktop and that monitor are the same area, so only one "Screen" entry is listed. The list is rebuilt when the display configuration changes. A selected monitor is matched by its display device name; if it disappears, the choice falls back to the first entry. Choosing another window resets a selected part. Full desktop requires a rectangular arrangement: if monitors of different sizes leave uncovered corners, it is rejected with an explanation (use a monitor or a portion instead).

### Mapping diagram

For every choice except the test area, the right panel shows a centered mapping diagram that fits within 420 by 318 logical pixels. In diagnostics mode, the diagnostic text sits above it.

- **Top:** each monitor drawn in its real arrangement and proportions, with a bezel, a stand under monitors on the bottom row, and a desktop thumbnail.
- **Bottom:** the touchpad at its declared physical aspect. If the device geometry is unavailable, a 1.6 placeholder is shown and labeled as such. Unused parts of the touchpad are darker.
- **Red:** the mapped region on the screens, the active part of the touchpad, and four connectors between their corners.
- **During capture:** each live finger is drawn as a colored dot on the touchpad and, when it is inside the used part, at its mapped position on the screen.

While idle, the region is re-resolved every 250 ms, so moving a target window updates the diagram. During a session the diagram shows the session's current bounds, including retargets. If the target is not configured or not available, the diagram shows the reason instead of a region. The thumbnail is drawn only in this panel; it is never logged or written to disk.

The desktop thumbnail refreshes once per second while the Pad2Touch main window is active in the foreground and shows the mapping diagram. It pauses when another window takes focus, when Pad2Touch is minimized, or while an area is being selected. A worker reads the desktop so the GUI can repaint and respond to input independently.

The window list contains visible, uncloaked, unowned top-level windows of other processes, refreshed whenever the list opens. Titles are shown for selection only; logs record the handle, mode and physical bounds, never window titles. Pad2Touch's own windows cannot be application targets. Selections are not persisted across launches.

### Touchpad area

The **Touchpad area** list uses the mapping terms from [Wacom's documentation](https://101.wacom.com/userhelp/en/ForceProportions_Full.htm).

| Choice | Touchpad used | Screen area used | Shape | Needs physical geometry |
|---|---|---|---|---|
| Force proportions (default) | The whole pad in Test area, a centered part for screen or window targets | Test area fits the pad, other targets use their full selected bounds | Undistorted | Yes |
| Full | All | All | Scaled independently on X and Y | No |
| Portion… | The area you draw on the touchpad diagram | All | Scaled independently on X and Y | No |

In **Force proportions** and **Portion…**, the mapping diagram shades the unused touchpad area and shows contacts only inside the active area. Pad2Touch ignores outside contacts for injection and releases injected contacts when they slide out. Windows can still use those fingers for its normal pointer input.

With **Force proportions**, resizing a window target recalculates the active part of the touchpad to match the new shape. For **Portion…**, click **Select pad area…** and drag on the touchpad diagram. You can use the selected portion with any screen target, including the test area.

In diagnostics mode, the used touchpad part is logged with each `TARGET`, `TARGET_UPDATE` and `TARGET_RESUME` record (`pad_fit=`, `pad_input=`) and shown in the diagnostics target line.

### Selection

Selection uses a topmost dimmed overlay in physical pixels; Esc, right-click, the emergency stop or Stop cancels it and keeps the previous selection. Screen rectangles must be at least 16×16 pixels, lie inside the virtual desktop and have their corners and center on a display; negative secondary-monitor coordinates are supported. Window rectangles are stored as fractions of the client area, so they scale with the window. After a selection, a click-through outline drawn just outside the mapped region shows it for 1.5 seconds; the outline is never shown during a session. The Touchpad setting below decides how the touchpad's shape fits the area.

During a session the worker re-resolves window targets every 16 ms:

- Moved or resized: active contacts are cancelled at their last positions, the new bounds apply, and a new all-fingers-up frame is required (`TARGET_UPDATE`).
- Minimized, hidden or cloaked (another virtual desktop): contacts are released and frames are ignored until the window returns (`TARGET_PAUSE` / `TARGET_RESUME`).
- Closed: the session stops (`TARGET_LOST`) and activation is disarmed.

Because Windows routes injected touch by screen position, every non-cleanup DOWN/UPDATE point is hit-tested first; a point covered by another window cancels the sequence (`INJECT_BLOCKED`) instead of touching the wrong application. The diagnostics line also samples a 3×3 grid every 250 ms and reports `COVERED at n/9 sample points` as an early warning. Starting or enabling a shortcut warns if the target runs at a higher integrity level, because UIPI may reject injected input there.

The Foreground guard only applies while Pad2Touch itself is foreground, so it is normally inactive while you work in a target application; Background guard is the relevant option there.

`mapping::MappingTarget` carries the configuration; `ResolvedTarget` adds physical bounds, the hit policy and the physical aspect so the worker can re-resolve. `mapping::Tracker` is the pure state machine behind retarget/pause/resume/stop decisions.

`control::Controller` owns activation intent independently of native controls. `experiment::Session` owns worker lifecycle and logging. The GUI owns the persistent window and receiver dispatch, reusing the CLI's parser, injector, pointer handling, and diagnostic renderer.

## Window layout and repainting

The window has a fixed size: no sizing border and no maximize button (it can still be moved and minimized). The client area is 820 by 528 logical pixels, or 820 by 556 in diagnostics mode to make room for the log path. The window scales with the monitor DPI. Non-client borders are calculated separately using AdjustWindowRectExForDpi. The toggle shortcut and window lists stay visible and are disabled when they do not apply. The preview group aligns with the top and bottom of the settings groups. A single row below it shows the capture state, relevant details, emergency stop hint, and capture buttons.

Native labels, group boxes and read-only status fields use the system COLOR_BTNFACE brush through the control-color messages. Editable/selectable input surfaces retain their system theme.

The window avoids flicker by never showing a half-painted state. The main window uses WS_CLIPCHILDREN, so the dialog background is never erased under a control that then has to repaint itself. Group boxes, which do not paint their own interior, are opaque, sit at the bottom of the sibling Z-order and paint the background, frame and caption together through a back buffer; sibling clipping keeps them off the controls inside them. The receiver paints through its own back buffer. WS_EX_COMPOSITED was tried and rejected: under DWM it left controls blank while the window changed size.

Layout batches positions with DeferWindowPos and only moves windows whose bounds change, using Windows' normal invalidation. Unchanged controls are not repainted. Screen-coordinate changes refresh the displayed diagnostic rectangle; each running injection session retains its own immutable target and existing movement/resize stop behavior. The message font comes from system non-client metrics for the current DPI.

During a session the live contact preview repaints when input arrives rather than on the GUI's 30 ms timer, which limited it to about 32 frames per second at an uneven pace. After each touchpad frame the capture worker posts one message to the panel, which invalidates it. A flag keeps at most one such message queued, and WM_PAINT coalesces frames that arrive faster than the panel paints, so the preview follows the report rate up to what the panel can paint. Touches received by the diagnostic receiver also invalidate it directly. The timer still updates status and controls.

## Physical shape and device information

In **Test area** with **Force proportions**, the canvas is centered and sized to the selected touchpad's declared physical X/Y dimensions, normalized with HID units and signed unit exponents. Both the preview and the injection target use this rectangle, with at most pixel-rounding error. Logical coordinate maxima alone are never used as a physical aspect ratio, since X and Y coordinate densities can differ. The physical-capture CLI receiver also preserves this aspect. Synthetic API scenarios have no physical touchpad geometry.

The HID descriptor's logical ranges, physical lengths, counts/mm, equivalent coordinate DPI and field bit widths are available from `--probe`. With `--diagnostics`, the GUI shows physical size, width/height, logical X/Y ranges and DPI even before starting capture. These are declared effective sensor dimensions, which can differ from outer glass or vendor-reserved areas. Field bit width describes report storage, not guaranteed measurement accuracy.

If a device omits valid physical dimensions/length units or declares conflicting coordinate geometry, the GUI shows it as unavailable and does not invent an aspect. Raw-only capture remains available; native injection is blocked until valid physical geometry is available.

Observed Hz distinguishes raw HID reports/s from assembled complete frames/s on the host over a recent active window. Hybrid report fragments must not be counted as separate touch frames. Scan cadence is independently estimated from complete active frames' 16-bit PTP Scan Time values (100 microseconds per tick), using median positive deltas with wraparound handling. Idle gaps, resets and insufficient samples are excluded; -- means unavailable rather than a claimed zero or nominal rate. This does not establish USB/I2C polling rate or end-to-end latency.
