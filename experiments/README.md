# Isolated settings-cache experiment

These developer tools investigate an undocumented Windows interface. They are separate from Pad2Touch's default capture path. They do not implement a production global-capture mode, a GUI, hotkeys, drivers, or process injection. Use the ordinary interactive desktop under the same user account as Explorer.

The GUI's explicit **Background guard (experimental)** selection embeds this transaction and its native helper through a separate adapter. Its `session` mode retains the same journal, lease, snapshot, readback, and watchdog path while the GUI owns Raw Input capture. It monitors the GUI owner's process identity, accepts a stop marker after input cleanup, and signals expiry at 120 seconds before restoration. This GUI mode accepts only the locally validated build 26300.9974 and remains off by default. The original bounded `apply` experiment is still available independently.

## Build and read-only inspection

```powershell
New-Item -ItemType Directory artifacts\experiments -Force | Out-Null
rustc --edition 2021 -D warnings -O experiments\gesture-cache-probe.rs -o artifacts\experiments\gesture-cache-probe.exe
.\artifacts\experiments\gesture-cache-probe.exe
.\artifacts\experiments\gesture-cache-probe.exe --read-cache
.\experiments\Test-BackgroundGestureSettings.ps1 -Mode probe
.\experiments\Test-BackgroundGestureSettings.ps1 -Mode self-test
```

The default native probe activates IImmersiveSettingsCache only. `--read-cache` reads the four candidate IDs reported by the [original author](https://stackoverflow.com/questions/73331905/disable-gestures-through-registry). It does not change preferences. The PowerShell probe additionally reads exactly four current-user registry values and the Windows build. All outputs and recovery data are local diagnostics and belong under ignored `artifacts/`.

The native helper's explicit `--reload-four` option calls OnSettingChanged for only `0x0F`, `0x10`, `0x11`, and `0x12`, checks each HRESULT, then reads cache values. It performs no registry writes itself. Do not treat notification success or cache readback as proof of physical gesture suppression. The ABI and IDs are community observations rather than a Microsoft compatibility contract; see [the research document](../docs/background-gesture-feasibility.md).

GUI sessions with injection additionally pass `-SuppressTouchscreen` to the transaction script. This build-bound option records and temporarily disables the live touchscreen three-/four-finger system-gesture state, preserves the persistent desktop preference, and restores through the same journal/watchdog. It is not enabled for the raw-only `apply` experiment. A read-only `probe -SuppressTouchscreen` includes the live state and persistent preference snapshot. See [GUI gesture behavior and native test coverage](../docs/gui.md#background-guard).

## Explicit, bounded mutation test

First review the read-only output and the actual Windows build. The following template intentionally requires substituting a build number and a new local directory:

```powershell
cargo build --release --locked
.\experiments\Test-BackgroundGestureSettings.ps1 -Mode apply -ExpectedBuild YOUR_REVIEWED_BUILD -Seconds 75 -RunDirectory artifacts\experiments\YOUR_NEW_EXPERIMENT
```

For a physical test, add `-WaitForStart`: an operator-controlled dialog appears before any settings change, and the timed test starts only after OK. A start tone and a successful-restoration tone distinguish the interval from ordinary restored operation. Begin the gestures promptly and lift all fingers before the ending tone. `-CaptureExecutable` can pin a previously validated capture binary while unrelated development continues in the checkout.

`-ExpectedBuild` is an operator acknowledgement of the exact experimental build, not an automatic compatibility check. Unrecognized registry types, missing prerequisites, failed cache reads, or mismatching existing registry/cache values prevent mutation. Absent registry values are preserved as absent; the snapshot also records their pre-experiment cached defaults.

The script preserves the existence, type, and original DWORD bytes of only these names under `HKCU\Software\Microsoft\Windows\CurrentVersion\PrecisionTouchPad`:

- `ThreeFingerTapEnabled`
- `FourFingerTapEnabled`
- `ThreeFingerSlideEnabled`
- `FourFingerSlideEnabled`

Before writing, it flushes `original.json` and starts an independent recovery process. That process must acknowledge readiness. A durable mutation marker precedes the four writes. The script sets the four values to zero, checks registry readback, notifies the four cache IDs, and requires four zero cache reads. Directional/custom bindings and unrelated preferences are not edited.

Once active, it runs a raw-only, minimized Pad2Touch capture for the requested duration. Keep another application foreground, repeat three-/four-finger swipes and taps, and include single-/two-/five-contact movement and complete releases. Observe shell behavior separately from the captured input.

Normal completion and failures after mutation request exact restoration. The recovery process also attempts restoration when its parent exits or its deadline expires. The original existence/type/bytes and cache values are checked after restoration; originally missing values are removed again instead of being restored as enabled DWORDs. Changed external values are retained and reported for review. A per-run mutex serializes normal and watchdog restoration.

The same per-run lock also protects the mutation marker and four registry writes. The writer checks the recovery marker, deadline, and watchdog under that lock, so an expired or restored run cannot write disabled values afterward. A persistent lease under the current user's local application data (`PadBridge/ExperimentalGestureCache/active.json`) records the active journal across checkout/process lifetimes. A new apply is rejected while an earlier lease lacks verified restoration; the original settings are re-read only after this check under a global mutex. A damaged lease fails closed and requires manual recovery review. Recovery artifacts are local and never part of a source commit.

Deadline normalization preserves UTC and subsecond precision for both ISO strings and DateTime objects returned by PowerShell JSON deserialization. The pure self-test covers both representations. A successful settings-cache notification, a completed capture timer, and verified restoration are separate from the operator's physical suppression result; test activity must overlap the recorded active interval.

After the experiment, confirm `restored.json` reports exact registry/cache restoration and check ordinary gestures physically. Preserve the entire local directory until recovery is verified. To retry recovery explicitly:

```powershell
.\experiments\Test-BackgroundGestureSettings.ps1 -Mode restore -Journal artifacts\experiments\YOUR_EXPERIMENT\original.json
```

Do not run concurrent experiments, edit preferences during one, replace the helper binary mid-run, or start a new experiment before an earlier journal has been recovered. A change to the same zero value cannot be distinguished from the experiment's own write. The watchdog is a separate best-effort process, not a guarantee against logoff, machine shutdown, killed recovery processes, Explorer failure, or incompatible undocumented interfaces. Forced-termination recovery needs its own test. There is no installed persistent recovery service or automatic startup recovery.

The implementation is independently written using interface facts with source attribution. No external script or GPL implementation is embedded. These tools do not install or change the user's PowerShell execution policy. Normal CLI capture does not enable them; GUI use requires explicitly selecting Background guard.
