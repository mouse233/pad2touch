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

## Optional native API exercise

```powershell
.\scripts\Run-PadBridge.ps1 -Mode self-test -Contacts 5 -Seconds 40
```

This command submits **real native touch input** using synthetic coordinates. It covers dragging, holds, quick taps, two-finger pinch/expand, partial release, replacement, and concurrent contacts. A successful synthetic exercise does not replace physical touchpad testing.

## Interpret logs

```powershell
.\scripts\Analyze-Run.ps1 -RunDirectory artifacts\YOUR_RUN
```

Review `summary.txt` and `analysis.json`, including parse failures, incomplete frames, injection errors, log loss, receiver lifecycle errors, remaining contacts, and cleanup status. Injection batch counts need not equal received message counts because Windows can coalesce UPDATE messages.

The verifier checks complete lifetimes and ordered coordinates. `passed=true` means that these log checks succeeded. It does not by itself prove every manual scenario, visual behavior, edge coordinate, hold duration, or compatibility requirement passed.

Record each manual outcome locally as passed, failed, or not tested, with the observation and any scope exception. Preserve the distinction between a confirmed failure, an untested boundary, and a hardware-specific reserved region. Review device paths, usernames, local directories, and screen content before sharing diagnostics.
