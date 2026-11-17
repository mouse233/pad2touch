# Testing guide

This guide contains reusable procedures, not results from a particular machine. Keep observations, acceptance decisions, screenshots, raw input, and generated reports under ignored local directories such as `artifacts/`.

## Offline checks

```powershell
cargo test --locked
cargo clippy --all-targets -- -D warnings
cargo fmt -- --check
cargo build --release --locked
```

Default tests do not open a receiver window or call the native touch-injection APIs. They exercise decoding, report assembly, mapping, complete contact frames, stable IDs, partial releases, slot replacement, retries, cancellation, and log verification through a fake transport.

Optional private recording tests are documented in [the fixture directory](../tests/fixtures/README.md). A clean checkout must build and run default tests without these recordings.

## Prepare a physical test

1. Use an ordinary interactive Windows desktop and build the Release executable.
2. Run `scripts\Run-PadBridge.ps1 -Mode probe` and select a collection with `-Device N` if needed.
3. Keep the dedicated receiver visible and stationary. Native touchpad mouse behavior and system gestures remain enabled.
4. Touch the pad and lift all fingers once to provide a clear frame. Wait for `ARMED` before beginning a new sequence.
5. Use a new output directory for every run. The wrapper creates timestamped directories automatically.

Some hardware reserves special regions for vendor controls. Record such coverage exceptions separately from confirmed mapping defects. A device-specific exception does not establish compatibility for another touchpad.

## System-gesture conflicts and rendering

### Isolate background Raw Input

```powershell
.\scripts\Run-PadBridge.ps1 -Mode capture -Background -Contacts 5 -Seconds 75
```

This raw-only diagnostic starts the receiver minimized without requesting foreground. Do not add `-Inject`. Keep another ordinary application foreground, touch and lift all fingers once, then repeat single-finger movement, two-finger movement/pinch, three-/four-/five-contact sequences, partial lifts, and complete release. Leave all fingers lifted at the end. Windows mouse and shell gestures remain enabled; this test measures receipt only. A visible receiver without focus is also a valid background condition; minimization is a convenient way to maintain that condition.

Each selected-device raw report records `foreground_scope` from the actual foreground process and `delivery_code` from WM_INPUT. Code 1 means RIM_INPUTSINK. Full frames are classified from all contributing fragments; mixed or unknown focus frames are not background evidence. `CAPTURE_FOCUS` records transitions without storing other applications' titles or paths.

Review the background counters in `summary.txt`: raw reports, RIM_INPUTSINK reports, complete frames, peak contacts, coordinate changes, contact starts/ends, clear frames, interruptions, and contacts remaining. Lifetime counting requires an all-fingers-up baseline in the same background interval. Contacts crossing a foreground transition or parse failure are classified as interrupted rather than counted as complete background lifetimes. Peak count alone does not establish release delivery. Compare the recorded cases with the operator's actual actions and inspect parse errors, incomplete frames, and log loss. These counters are observations, not an automatic hardware acceptance verdict.

See [background gesture feasibility](background-gesture-feasibility.md) for the separate suppression investigation and a settings-assisted experiment. Background receipt does not establish exclusive capture.

### Foreground gesture ownership

To temporarily claim three-/four-finger touchpad gestures while the PadBridge receiver is foreground:

```powershell
.\target\release\padbridge-phase0.exe --gesture-support
.\scripts\Run-PadBridge.ps1 -Mode capture -Inject -SuppressSystemGestures -Contacts 5 -Seconds 180
```

Check that the receiver shows `3/4-finger guard: ACTIVE (foreground)`. Repeat three-/four-finger swipes and taps, confirm independent contact trails, and observe whether task switching or desktop actions occur. Do not switch to a chat or another application to report progress while testing. Wait until capture ends before giving feedback. A visible topmost window is not necessarily the foreground window: `INACTIVE (background)` means that the controller is ineligible. End capture, then verify ordinary Windows touchpad gestures work again in another foreground application. Gesture-controller activation/readback alone does not establish that physical gesture interception worked.

For isolation, run a separate capture without injection:

```powershell
.\scripts\Run-PadBridge.ps1 -Mode capture -SuppressSystemGestures -Contacts 5 -Seconds 30
```

No received touch trails are expected in this run. Review `GESTURE_EVENT`, `GESTURE_HANDLER_ERROR`, and `WINDOW_STATE` records and compare timestamps. Then repeat with `-Inject`. A system action during raw-only capture cannot have been caused by PadBridge's injected touches. A failure only with injection calls for separate investigation of touchscreen gesture handling. If needed, open `ms-settings:devices-touch` and manually compare the Windows three-/four-finger **Touch** preference on and off, restoring the original preference afterward. PadBridge does not modify this setting.

The option does not modify Windows preferences. If unavailable, the requested run fails before capture. Do not report suppression as working simply because capture without the option succeeds. If conflicts persist, distinguish physical touchpad shell gestures from Windows' separate three-/four-finger touchscreen gestures; see the [Microsoft explanation](https://learn.microsoft.com/en-us/troubleshoot/windows/win32/finger3or4-touch-interaction-no-longer-works).

The receiver uses double buffering. Observe it during idle updates, active trails, uncovering, and stop/drain: it should present complete frames rather than repeatedly flashing a cleared background. A single screenshot cannot establish absence of flicker over time. Rendering allocation/presentation failures appear as RENDER_ERROR in local logs.

## Single-contact procedure

```powershell
.\scripts\Run-PadBridge.ps1 -Mode capture -Inject -Contacts 1 -Seconds 120
```

Check:

- Starting while a finger is already touching, then lifting it before beginning capture.
- The usable area's corners and center, noting any hardware-reserved areas.
- Continuous lines, circles, diagonal movement, and long drags.
- A stationary hold lasting at least 30 seconds.
- Lifting, waiting at least three seconds, and touching again.
- Repeated quick taps with paired DOWN and UP events.
- Adding a second finger and lifting the selected finger: the single-contact mode should end its sequence rather than jump to the remaining finger.

## Multitouch procedure

```powershell
.\scripts\Run-PadBridge.ps1 -Mode capture -Inject -Contacts 5 -Seconds 180
```

Check two-finger movement, pinch/expand, one stationary finger while another moves, partial release, and repeated replacement of one finger while retaining another. Then check three, four, and five simultaneous contacts, followed by simultaneous release.

Look for stable IDs, independent trails, the expected active count, retained contacts during a partial lift, and no accumulated contacts after repeated sequences. The two-finger distance ratio and demonstration box should respond to received coordinates. Compatibility with another application's gesture recognizer needs a separate test.

## Shutdown procedure

```powershell
.\scripts\Run-PadBridge.ps1 -Mode capture -Inject -Contacts 5 -Seconds 30
```

Hold multiple contacts and press Esc. Repeat with duration expiry while contacts remain active. Check that final UP events arrive and both sides finish with zero contacts and no cleanup pending. An ordinary exit after fingers have already lifted does not cover this case.

Occlusion, minimization, window movement, display changes, and device removal are cancellation tests. Keep them separate from clean gesture runs, since intentionally triggered runtime errors require manual interpretation. Forced process termination is not a substitute for cooperative shutdown testing.

A runtime safety stop keeps the result window until explicit close or the original duration expires. If minimized, restore it from the taskbar and inspect the stop reason; it must remain stopped and release contacts rather than automatically rearm. Check that the gesture controller is disabled even while the result window is retained.

## Optional native API exercise

```powershell
.\scripts\Run-PadBridge.ps1 -Mode self-test -Contacts 5 -Seconds 40
```

This command submits **real native touch input** using synthetic coordinates. It covers dragging, holds, quick taps, two-finger pinch/expand, partial release, replacement, and concurrent contacts. A successful synthetic exercise does not replace physical touchpad testing.

Add `-SuppressSystemGestures` to check coexistence with the controller. Start with `-Contacts 1` to avoid the separate three-/four-finger touchscreen gesture layer, then test capacity 5 independently. Run native checks on the ordinary interactive desktop; restricted execution contexts may reject injection or prevent receiver delivery. Keep input and focus undisturbed during an automated sequence.

## Interpret logs

```powershell
.\scripts\Analyze-Run.ps1 -RunDirectory artifacts\YOUR_RUN
```

Review `summary.txt` and `analysis.json`, including parse failures, incomplete frames, injection errors, log loss, receiver lifecycle errors, remaining contacts, and cleanup status. Injection batch counts need not equal received message counts because Windows can coalesce UPDATE messages.

The verifier checks complete lifetimes and ordered coordinates. `passed=true` means that these log checks succeeded. It does not by itself prove every manual scenario, visual behavior, edge coordinate, hold duration, or compatibility requirement passed.

The wrapper saves the audit JSON and emits a warning for failed touch-delivery checks by default. This does not convert a failure to a pass. Use `-Strict` on `Analyze-Run.ps1` or `Run-PadBridge.ps1` when an automated workflow needs a terminating failure. Native `--verify-log` retains its nonzero exit code for failed audits. Command errors, missing files, and invalid arguments remain errors.

Record each manual outcome locally as passed, failed, or not tested, with the observation and any scope exception. Preserve the distinction between a confirmed failure, an untested boundary, and a hardware-specific reserved region. Review device paths, usernames, local directories, and screen content before sharing diagnostics.
