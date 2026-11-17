# Background touchpad capture and gesture suppression

This document analyzes public Windows interfaces and describes reproducible experiments. It is not a hardware test report and does not claim that an untested suppression method works.

The intended behavior is for PadBridge to keep receiving Precision Touchpad contacts while another application is foreground, convert them into native touch input, and avoid simultaneous Windows touchpad actions while replacement is active. These are separate requirements: receiving input does not establish exclusive ownership of it.

## Scope and constraints

The current design stays in user mode, uses native touch injection, and does not install a driver, inject code into a target process, modify firmware, or write device Feature Reports. Ordinary capture does not change system touchpad preferences. A proposed preference-changing mode would need an explicit scope, verified restoration behavior, and its own acceptance tests.

This review did not find a documented public API that grants a background application temporary, session-scoped ownership of three- and four-finger Precision Touchpad shell gestures while preserving the raw contact stream. **Not finding such an API is not proof that every possible implementation is impossible.** The experimental settings-cache route below is a concrete undocumented candidate for further validation. Its existence does not change the documented API limits or establish production reliability.

## Documented facts

### Background receipt is supported by Raw Input

`RIDEV_INPUTSINK`, with a target window, requests input even when the registering application is not foreground. PadBridge uses this registration. It must still be tested on the actual device: a successful registration alone does not demonstrate that every physical contact and release reaches the parser during a focus change. Neither this flag nor the arrival of raw reports promises to prevent Windows from interpreting the same contacts. See [RAWINPUTDEVICE](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-rawinputdevice).

`RIDEV_EXINPUTSINK` is a conditional background fallback: foreground Raw Input handling takes priority. It is not an exclusive-capture flag. `RIDEV_NOLEGACY` is defined for mouse and keyboard devices and concerns legacy messages for the application. `RIDEV_CAPTUREMOUSE` concerns mouse-click activation and requires mouse NOLEGACY registration. These flags do not provide a documented PTP shell-gesture suppression mechanism. See the same [registration contract](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-rawinputdevice).

### The gesture controller is foreground-process scoped

`Windows.UI.Input.TouchpadGesturesController` lets an eligible foreground process handle supported multi-contact touchpad gestures instead of the shell. The documentation expressly excludes controllers in background processes. Creating more controllers, adding a message-pump thread, or leaving a receiver visible does not extend that documented scope. Its thread needs a message pump, but the pump requirement is distinct from foreground eligibility. See [TouchpadGesturesController](https://learn.microsoft.com/en-us/windows/win32/input-precisiontouchpad/touchpadgesturescontroller).

Consequently, successful foreground interception is evidence for a foreground scenario only. It cannot establish suppression while an unrelated target application is foreground.

### Public dynamic touchpad settings do not expose these assignments

Windows 11 version 24H2 provides `SPI_GETTOUCHPADPARAMETERS` and `SPI_SETTOUCHPADPARAMETERS`. The published `TOUCHPAD_PARAMETERS_V1` writable settings include taps, tap-and-drag, two-finger tap, right-click zones, two-finger pan and zoom, and other touchpad preferences. It has no three- or four-finger swipe/tap assignment fields. The enabled/active fields are status information and are ignored by SET. See [TOUCHPAD_PARAMETERS_V1](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-touchpad_parameters_v1).

Header inspection of Windows SDK version 10.0.26100.0 also finds a `TOUCHPAD_PARAMETERS_V2` declaration. It extends V1 with `button1Supported`, `button2Supported`, and `button3Supported` capability bits, not multi-finger gesture assignments. This is a declaration comparison, not a claim that V2 works on every runtime. Reserved fields must not be repurposed.

Microsoft documents dynamic preference changes through `SystemParametersInfo` and says they should follow explicit user intent, under the user's account. Its published tuning settings do not establish a supported live-reload contract for the commonly suggested `ThreeFingerSlideEnabled` and `FourFingerSlideEnabled` registry values. Direct registry recipes are therefore not treated as a supported session controller. The undocumented cache notification candidate is evaluated separately below. See [Precision touchpad tuning](https://learn.microsoft.com/en-us/windows-hardware/design/component-guidelines/touchpad-tuning-guidelines).

### Device configuration reports are not a gesture-only switch

The standard configuration collection describes two relevant Feature Reports:

| Report or field | Documented effect | Implication for this requirement |
|---|---|---|
| Input Mode, usage `0x52` | Selects mouse collection reporting or PTP collection reporting. | Selecting mouse mode does not preserve the required PTP multi-contact stream. |
| Surface Switch, usage `0x57` | Selects whether surface contacts are reported. | Disabling surface reports also removes the contacts PadBridge needs. |
| Button Switch, usage `0x58` | Selects whether button state is reported. | Disabling button reports does not describe disabling Windows gestures recognized from surface contacts. |

These documented controls do not describe a mode that sends raw contacts to an application while withholding the same surface data specifically from the Windows gesture recognizer. No supported Feature parameter for that narrower purpose was found in this review. PadBridge does not probe writable Feature behavior or alter these reports. See [Configuration Collection](https://learn.microsoft.com/en-us/windows-hardware/design/component-guidelines/touchpad-configuration-collection).

## Other candidate approaches

| Candidate | Assessment |
|---|---|
| Low-level mouse hook | Its documented callbacks contain mouse move, button, and wheel messages. This does not establish access to, or cancellation of, every PTP shell gesture. Correlating those messages with a particular contact stream would be an additional unverified assumption. No global hook is implemented for this purpose. See [LowLevelMouseProc](https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelmouseproc). |
| `SetGestureConfig` | Configures Windows Touch gesture messages for a specified window. It does not document a global PTP shell-gesture switch. See [SetGestureConfig](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setgestureconfig). |
| Direct exclusive HID access | Windows opens the PTP top-level collection for system use with exclusive access. Raw Input observation should not be confused with acquiring that device exclusively. See [Top-level collections opened by Windows](https://learn.microsoft.com/en-us/windows-hardware/drivers/hid/top-level-collections-opened-by-windows-for-system-use). |
| Customization filter driver | Microsoft's device-customization guidance discusses controlling delivery to the Windows gesture processor in a filter-driver design. Installing such a component is outside the project's current constraints. See [Experience Customization](https://learn.microsoft.com/en-us/windows-hardware/design/component-guidelines/touchpad-experience-customization). |
| Force PadBridge to retain foreground | Does not meet the requirement that other applications remain usable in the foreground. It is not a substitute for background suppression. |

## Experimental route: refresh the Immersive Settings Cache

**Status: undocumented candidate, not a verified suppression or recovery implementation.** It changes user preferences and then asks Explorer to reload selected cached settings. It is conceptually different from exclusive input capture: Windows would remain configured to ignore those gesture actions while PadBridge independently observes raw contacts.

The answer by **co_init_ex** on [Disable Gestures through registry](https://stackoverflow.com/questions/73331905/disable-gestures-through-registry) describes this COM path for Windows 11 25H2:

| Item | Reported identifier or shape |
|---|---|
| Shell class | `CLSID_ImmersiveShell`: `c2f03a33-21f5-47fa-b4bb-156362a2f239` |
| Service | `SID_ImmersiveSettingsCache`: `53660488-8855-460b-a9ab-5cfc6b5012ca` |
| Cache interface | `IID_IImmersiveSettingsCache`: `4214f6fa-eb36-4e2f-9ca2-23fdc1832df7` |
| Activation | COM initialization, `CoCreateInstance` with `CLSCTX_LOCAL_SERVER` to obtain `IServiceProvider`, then `QueryService` for the cache interface |
| Notification | `HRESULT OnSettingChanged(int32 settingId)`, the first method after the three IUnknown slots |
| Subsequent reads | `GetBOOL(settingId, BOOL*)`, then `GetDWORD(settingId, DWORD*)`; both output types are 32 bits |

The reported interface is IUnknown-based COM, not WinRT IInspectable or IDispatch. Its notification method is named `OnSettingChanged`, not `NotifyChanged`. All ABI details remain reverse-engineered observations. Standard [COM activation](https://learn.microsoft.com/en-us/windows/win32/api/combaseapi/nf-combaseapi-cocreateinstance) and [QueryService ownership](https://learn.microsoft.com/en-us/windows/win32/api/servprov/nf-servprov-iserviceprovider-queryservice%28refguid_refiid_void%29) rules still apply, including checking HRESULTs and releasing acquired interfaces. The sample's successful service acquisition does not establish that every subsequent notification succeeded; each call needs an independent result check.

### Version mapping is an experimental prerequisite

An independent first-person [NVDA developer discussion by gexgd0419](https://github.com/nvaccess/nvda/discussions/19841) describes the same cache interface and reports these setting IDs:

| Reported Windows range | ThreeFingerTapEnabled | FourFingerTapEnabled | ThreeFingerSlideEnabled | FourFingerSlideEnabled |
|---|---:|---:|---:|---:|
| Windows 10 1511-2004 | `0x09` | `0x0A` | `0x0B` | `0x0C` |
| Windows 11 21H2 | `0x0E` | `0x0F` | `0x10` | `0x11` |
| Windows 11 22H2-25H2 | `0x0F` | `0x10` | `0x11` | `0x12` |

These are community observations, not a Microsoft compatibility promise. The discussion also warns that a successful notification return does not prove Explorer recognized or applied a setting. A prototype should reject unverified builds instead of guessing IDs or broadcasting a broad range that includes unrelated settings. Capability checks, cache readback, and physical behavior checks are distinct acceptance steps.

### Proposed narrow experiment and restoration contract

The following is PadBridge's proposed experiment design, rather than an observed result or a copy of the referenced implementations:

1. Start with a read-only probe: record the OS build, activate the service, and inspect only the four relevant current-user values and corresponding cache entries. Do not notify or change settings at this stage. An unexpected type, failed cache read, or mismatch between an existing registry value and the cache prevents mutation. A missing value must be represented as absent with its original cached default preserved; it must not silently be treated as an enabled DWORD. An unverified build needs an explicit, isolated experiment rather than automatic activation.
2. Before any opt-in write, preserve each value's exact existence, registry type, byte length, and bytes in a local recovery journal. Limit access to the four names under `HKCU\Software\Microsoft\Windows\CurrentVersion\PrecisionTouchPad`. Do not export the entire user hive or collect custom keyboard bindings. See [RegQueryValueExW](https://learn.microsoft.com/en-us/windows/win32/api/winreg/nf-winreg-regqueryvalueexw).
3. In the explicitly bounded experiment, change only these four top-level gesture selections to DWORD zero, preserve all directional/custom assignment values, and send only their four candidate cache notifications. This includes creating an absent value temporarily only when the snapshot and recovery path preserve its absence. Check every write and notification separately; do not restart Explorer or use a broad settings broadcast as a fallback.
4. With touch injection and the foreground gesture controller both disabled, keep another application foreground and verify background contact delivery alongside the absence of the targeted shell actions. Registry or cached values alone do not establish the physical outcome.
5. Restore the exact old existence, types, and bytes, notify the same four IDs, read back the registry/cache, and repeat the baseline physical gestures. Remove a temporarily created value if it was originally absent. Do not restore all enabled values to one: original action selections may differ. [RegSetValueExW](https://learn.microsoft.com/en-us/windows/win32/api/winreg/nf-winreg-regsetvalueexw) accepts both type and byte data.

Before production use, the design also needs rollback for partial writes, a durable recovery record written before mutation, handling of Explorer restarts or unavailable COM services, and explicit recovery after process failure. Restoration should detect a user or another process changing an owned value during capture rather than silently overwriting that newer preference. Exact preservation and preservation of concurrent edits are separate requirements; neither is supplied by the COM notification interface. Recovery data stays local and excluded from source control.

This route should be an explicit experimental option. It is not scoped by foreground ownership, but the resulting preference changes are user-wide and can persist after termination until restored. It does not suppress native mouse motion or every other touchpad action, and does not address injected touchscreen gesture processing. A successful raw-only experiment would still require separate native-injection and failure-recovery tests.

The independent [prototype tools](../experiments/README.md) provide read-only inspection and an explicit build-bound, limited-duration transaction with a recovery journal and separate watchdog. Their presence is not a compatibility or hardware acceptance claim. They are not enabled by normal PadBridge capture.

### Reference and license boundary

The sources above supply technical observations, not a permissive implementation to import. Stack Overflow's [licensing policy](https://stackoverflow.com/help/licensing) assigns CC BY-SA 4.0 to contributions from May 2, 2018 onward, including this answer. The related [NVDA pull request](https://github.com/nvaccess/nvda/pull/19823) is an unmerged experimental reference; its touchpad module declares GPL coverage, and the repository's [authoritative license](https://github.com/nvaccess/nvda/blob/master/copying.txt) is GPL version 2 or later with stated exceptions. No third-party implementation or external script is included or executed here. An independent PadBridge prototype should use the minimal required interface facts and its own implementation, with source attribution retained.

## Optional manual comparison experiment

This procedure is a manual control experiment for operating-system preferences. It is not a requirement to abandon the experimental programmatic route above, and it is not an implemented automatic capture mode. It uses the Windows settings interface rather than assuming registry writes alone are sufficient.

1. Record the current three- and four-finger swipe and tap assignments so the exact original selections can be restored. Preserve custom directional assignments if present. Keep this record local.
2. Start a new raw-only PadBridge capture, without touch injection and without the optional foreground gesture controller. Focus another ordinary application and keep it foreground while performing single-finger movement, multiple-contact movement, three-/four-finger swipes and taps, partial lifts, and all-fingers-up. Record the baseline shell behavior and receipt of the physical contacts separately.
3. In **Settings > Bluetooth & devices > Touchpad**, set both **Swipes** and **Taps** to **Nothing** for both three- and four-finger gestures. Inspect advanced/custom assignments where the Windows version exposes them. This is the Touchpad page; the separate Touch setting governs touchscreen gestures.
4. Start a fresh raw-only capture. Again keep another application foreground and repeat the same physical actions. Confirm both that the unwanted shell actions do not occur and that PadBridge receives complete contacts, coordinates, and release transitions in the background.
5. Restore every original assignment through Settings, including custom assignments. Repeat a short baseline check to confirm ordinary Windows gestures work again.

Windows provides user-facing gesture preferences; an official example describes selecting Nothing for a multi-finger tap action. Exact settings labels and available choices vary, so inspect the actual interface rather than relying on coordinate automation. See [Microsoft's touchpad settings example](https://support.microsoft.com/en-us/windows/experience/prevent-windows-search-from-randomly-launching).

For each run, distinguish the receiver's foreground state from mere visibility or topmost status. Count background raw reports, complete frames, active contacts, transitions, parser failures, and releases. If the source supplies scan information, preserve it for continuity analysis. Shell inactivity without received contacts is not a successful bridge, and received contacts with simultaneous shell actions are not exclusive capture. Keep observations and acceptance results in ignored local reports.

## Interpretation and remaining boundaries

- A passing raw-only background test establishes receipt for the tested device, actions, and desktop conditions. It does not establish background suppression or arbitrary-device compatibility.
- A passing settings-assisted test establishes the effect of the tested preferences. Those are user-wide settings that persist until changed; the experiment does not prove an automatic per-session override, safe restoration after a crash, or coordination with a user changing settings during capture.
- Automatic preference switching would require a supported mutation mechanism, exact state preservation, readback and behavioral verification, conflict handling, and a recovery design. None is implied by documenting the manual experiment.
- Foreground controller interception remains useful within its documented scope. It does not satisfy the background ownership requirement.
- Touch injection and Windows' separate touchscreen gesture processing require their own tests. Raw-only capture deliberately removes that layer from this experiment.
- No conclusion here authorizes firmware changes, Feature Report writes, driver installation, process injection, or broad mouse/keyboard interception.
