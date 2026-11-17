# Architecture

PadBridge is a Windows user-mode bridge from Precision Touchpad HID reports to native touch injection. The current MVP has a native GUI with an embedded diagnostic receiver and Phase 3 targets: application windows, rectangles inside windows, and desktop rectangles. Persistent configuration and release-level recovery remain outside this implementation. The CLI receiver remains available.

## Modules

| Module | Responsibility |
|---|---|
| `main.rs` | Validate command-line options and dispatch capture or offline log verification. |
| `gui.rs` | Own native controls, device selection, global shortcuts, the embedded receiver, and repeated capture sessions. |
| `control.rs` | Track manual, Toggle, and Hold intent independently of asynchronous worker startup/shutdown. |
| `mapping.rs` | Resolve diagnostic, window, window-rectangle and screen-rectangle targets in physical pixels; hit policies; the dynamic target tracker. |
| `overlay.rs` | Desktop rectangle selection overlay and click-through target outline. |
| `device_info.rs` | Interpret declared HID length units, physical shape, logical ranges and coordinate density. |
| `report_rate.rs` | Distinguish observed raw delivery, complete frame delivery and PTP scan cadence. |
| `background_guard.rs` | Adapt the validated bounded settings-cache transaction to GUI sessions, retain recovery assets, and wait for restoration. |
| `capture.rs` | Attribute raw reports and complete frames to foreground/background intervals and count conservative background contact evidence. |
| `hid.rs` | Discover Raw Input collections, retrieve HidP capabilities and values, and validate aligned RAWHID buffers. |
| `decode.rs` | Interpret fields, assemble complete reports, and map absolute coordinates without Windows API dependencies. |
| `touch.rs` | Plan complete contact frames, assign stable slots, commit successful state, retry, and cancel through an abstract transport. |
| `injection.rs` | Implement the Win32 transport using hit testing, POINTER_TOUCH_INFO arrays, and InjectTouchInput. |
| `gestures.rs` | Optionally own three-/four-finger gestures through the published WinRT controller ABI, with capability checks and scoped disable/release. |
| `render.rs` | Own a reusable compatible DC and bitmap, and present complete frames with one BitBlt. |
| `scenarios.rs` | Generate deterministic logical scenarios for offline tests and the explicitly requested native API exercise. |
| `experiment.rs` | Own the input worker, receiver window, diagnostics, contact trails, and coordinated shutdown. |
| `verify.rs` | Audit batch completeness, contact lifecycles, and received coordinates without injecting input. |
| `logging.rs` | Write diagnostics through a bounded queue and count dropped records. |

Raw Input processing and injection run serially on one worker thread. The receiver processes pointer messages on a separate thread. Painting copies a bounded display snapshot and releases the statistics lock before GDI calls. A cached memory DC/bitmap receives background, text, trails, and active contacts; one BitBlt then presents the completed frame. The standalone CLI receiver suppresses background erasure. The native GUI parent paints the system dialog background, supplies the same system brush to native labels/group boxes, and batches child positions before invalidating all children. Its diagnostic child continues to use a back buffer. Buffer resources are replaced on size changes and released on shutdown. Log writing runs on its own thread; queue overflow is reported rather than treated as complete evidence.

## Optional foreground gesture ownership

`--suppress-system-gestures` can be used during physical capture or the explicit native self-test. Injection can be omitted in capture mode to isolate physical gesture handling. It creates a TouchpadGesturesController on the input worker, which has the message pump required by the API. All controller interfaces and subscriptions are created and released on that thread. The enabled controller declares ThreeFingerManipulations, FourFingerManipulations, ThreeFingerActions, and FourFingerActions (mask `0x1B`). It deliberately does not claim unrelated mouse input or five-finger actions.

The API routes these gestures to an eligible foreground controller instead of the shell. The controller acts as the destination; the bridge still consumes raw HID coordinates for touch injection. Its Enabled and SupportedGestures properties are read back after activation. Failed activation/readback triggers disabling and reports an error. Normal shutdown disables and verifies the controller before releasing it and uninitializing its WinRT apartment.

IUnknown-based typed delegates subscribe to PointerPressed, PointerMoved, PointerReleased, and GlobalActionPerformed before enabling. Pointer callbacks query ICoreWindowEventArgs, set Handled, and verify its readback; actions have no Handled property and are consumed by the registered action handler. Tokens are revoked before releasing the controller. Atomic reference counts own the delegates, and their shared data consists only of a thread-safe logger and event counter. Actual receipt, sampled movement, handler failures, and receiver activation/minimization are logged. The receiver distinguishes foreground eligibility from merely having enabled the controller. None of these observations guarantees that every physical or injected gesture was suppressed.

No user settings are saved or changed, so there is no registry snapshot or persistent preference to restore after termination. Ownership is process-foreground scoped: background controllers are ignored. The implementation uses the published UniversalApiContract v19 interface prefix from the Windows SDK and checks IsSupported at runtime because the API is experimental. It does not extend support to older systems by undocumented registry writes.

Touchpad gesture ownership is distinct from Windows' touchscreen gesture handling, including handling of injected PT_TOUCH input. Remaining conflicts must be diagnosed at the appropriate layer rather than attributing all lost receiver lifetimes to the physical touchpad.

## Input discovery and parsing

Discovery selects Digitizers / Touch Pad collections (`0D:05`). The selected raw device handle identifies the source throughout capture. `RIDI_PREPARSEDDATA` is cached, and HidP supplies report IDs, data indices, link collections, button capabilities, and logical coordinate ranges. Device-specific bit offsets are not hardcoded.

RAWHID buffers use aligned storage and checked sizes. Every report in `dwSizeHid * dwCount` is processed. Foreground WM_INPUT messages still pass through DefWindowProc for cleanup.

Registration uses RIDEV_INPUTSINK for background receipt. Each selected-device report records its WM_INPUT delivery code and a foreground-process comparison with the current process. A null/unavailable foreground is unknown, not assumed background. Hybrid frames inherit the merged scope of every contributing fragment; mixed frames are excluded from background evidence. A worker timer also samples focus while idle. No foreign window title, executable path, or ordinary keyboard input is collected for this attribution.

The raw-only `--background` diagnostic starts the receiver minimized without requesting activation. It is rejected with injection or self-test, since the existing fixed receiver must remain available for injection. Background evidence counts require a clear frame before tracking complete contact lifetimes. Focus transitions and parser failures invalidate active evidence instead of manufacturing successful releases; observed peaks and raw reports remain distinct from verified within-interval lifetimes. This does not confer exclusive touchpad ownership. See [the feasibility analysis](background-gesture-feasibility.md) for documented suppression boundaries.

The decoder groups contact fields by link collection. It rejects unsupported value arrays or layouts instead of manufacturing coordinates. Confidence=false contacts are retained in the input representation but excluded from injection.

## Complete input frames

Contact Count specifies the number of contact records, including explicit Tip=false releases. Hybrid frames can span several reports; continuation reports must share Scan Time with the opening report.

The assembler rejects duplicate IDs, a new frame before the previous frame completes, active orphan continuations, fragmented reports without Scan Time, and assembly waits exceeding 100 ms. A count-zero idle record can contain stale coordinates, but it must not contain an active tip.

If no raw report arrives for more than 500 ms, active injection is cancelled and a new clear frame is required. This is a conservative experiment-level timeout, not a claim that every device reports stationary fingers continuously. The implementation does not modify touchpad Feature Reports.

## Transactional contact planning

The planner maps raw Contact IDs to injection slots `0..capacity-1`, with capacity from 1 to 5. A planned submission contains both the outgoing frame and its candidate resulting state. The engine commits that state only after the transport succeeds.

In single-contact mode, the selected raw ID remains stable. When that finger leaves, the sequence ends and the planner waits for all physical tips to lift before selecting another finger.

In multitouch mode, all confident active contacts are tracked. Record ordering and raw ID magnitude do not change an existing slot. Exceeding capacity cancels the sequence instead of silently discarding contacts.

Every submission includes all previously active slots. When one finger lifts, the other contacts receive UPDATE in the same frame. A slot remains occupied throughout the frame carrying its UP and can only be reused after that frame succeeds.

UP, UPDATE, and DOWN are combined when free slots permit. If the union of departing and arriving contacts would exceed capacity, the planner first submits releases and retained-contact updates, then introduces new contacts. These two capacity-constrained submissions are not an atomic replacement.

Mapping uses the descriptor's logical ranges, normalization, `(length - 1)`, rounding, and clamping into a fixed physical-pixel rectangle. The process requests Per-Monitor DPI Awareness V2 before creating windows. Rotation, smoothing, and calibration are not part of the current mapper.

## Native injection and recovery

The transport initializes `InitializeTouchInjection(capacity, TOUCH_FEEDBACK_NONE)` and submits a POINTER_TOUCH_INFO array once per planned frame. Slot IDs remain inside the initialized capacity. Pressure, orientation, contact area, and custom timestamps are left unspecified.

| Action | Flags |
|---|---|
| Down | INRANGE, INCONTACT, DOWN |
| Move or hold | INRANGE, INCONTACT, UPDATE |
| Release | UP |
| Cancel | CANCELED, UP |

UP uses the last successfully injected position. If a contact has only received DOWN, an UPDATE is inserted before its UP. Cleanup sends a complete UPDATE frame followed by cancellation UP events. `INRANGE|UP` is avoided because it transitions into hover rather than ending the sequence.

`ERROR_NOT_READY` retries the same complete frame, with a 1 ms delay and at most 20 attempts. State is not advanced during retries. `ERROR_INVALID_PARAMETER` is handled as system cancellation of active injection, followed by waiting for a clear input frame.

Other failures trigger cleanup. If cleanup also fails, known contacts and `cleanup_pending` remain visible and new DOWN events are blocked. Subsequent work retries cleanup before rearming. A roughly 16 ms keepalive submits complete stationary frames.

Normal DOWN/UPDATE submissions are hit-tested with WindowFromPoint against the session's hit policy: exactly the diagnostic receiver, the target's top-level window or its children (`GA_ROOT`), or any window for screen rectangles. Cleanup bypasses this check so an existing sequence can still be ended after occlusion. For the diagnostic receiver, movement and minimization stop the session. Window targets are followed instead (next section). Device removal and display changes stop; reconnection is not implemented.

## Window and rectangle targets

All coordinates are physical desktop pixels. The process and the input worker thread run with Per-Monitor DPI Awareness V2, and target resolution refuses to run on any thread without it. A window's client rectangle is obtained by passing both client corners through ClientToScreen, so a DPI-virtualized (unaware or system-aware) window is converted consistently. Window rectangles are stored as fractions of the client area and scaled on every resolution.

The worker re-resolves window targets on its 16 ms timer and feeds `mapping::Tracker`: changed bounds produce Retarget, unavailable windows (minimized, hidden, DWM-cloaked, too small for the aspect) produce one Pause, the next available observation produces Resume, and a destroyed window produces Stop. `Engine::retarget` cancels active contacts at their cached coordinates before replacing the mapping rectangle and requires a clear frame, so no pointer lifetime spans two mappings. Frames are not injected while paused.

The input log records `TARGET` (mode, handle, receiver=embedded/external, bounds), `TARGET_UPDATE`, `TARGET_RESUME`, `TARGET_PAUSE`, `TARGET_LOST` and `TARGET_OCCLUSION`. The offline verifier checks every injected point against the current bounds. For external targets it cannot see the receiving application, so it audits injection-side lifetimes only and reports `receiver_audited=false`.

## Receiver and shutdown

The receiver only counts messages confirmed as PT_TOUCH by GetPointerInfo and also calls GetPointerTouchInfo. Each system pointer ID has an independent trail, color, and active position. Two received contacts drive a distance ratio and a small scale demonstration; this is a receiver diagnostic rather than proof of third-party gesture support.

Esc, WM_CLOSE, Ctrl+C, and duration expiry request worker shutdown. The worker attempts cancellation before publishing completion. The receiver then continues processing messages for approximately 300 ms before closing, allowing final UP events to arrive. Summaries include contacts remaining on both sides, cleanup failures, and log loss.

Runtime safety stops are distinct from a request to close the receiver. A moved or minimized target stops injection and records the specific reason, but leaves the result window available until explicit close or the original duration expires. It does not force foreground activation, restore a minimized window, or resume injection. Summary fields include stop reason, run status, actual gesture-event count, foreground losses, and minimizations. Normal close itself can cause a final activation loss; use event timestamps when correlating a fault.

Forced termination, process crashes, secure-desktop transitions, or rejected native cleanup can bypass this cooperative path. A successful API return alone does not establish correct receiver delivery or hardware acceptance.

## Offline log verification

Format-2 logs identify successful API submissions with INJECT_BATCH and record every slot with INJECT_OK. The verifier checks array counts, capacity, slot uniqueness, the complete previously active set, raw-ID stability, UPDATE requirements, and cached UP coordinates.

Windows-assigned pointer IDs differ from injection slots. Messages from different fingers can interleave, and UPDATE messages can be coalesced. Complete received lifetimes are therefore matched to injected lifetimes by ordered action/coordinate subsequences, using one-to-one matching when several contacts share coordinates.

Missing DOWN/UP events, incorrect coordinates, incomplete batches, logged errors, log loss, or unfinished shutdown records prevent a clean audit result. A consistent log is diagnostic evidence; manual acceptance remains a separate decision.

## Documentation and privacy

Repository documentation describes architecture and reusable testing procedures in English. The original [Chinese whitepaper](PadBridge.md) is retained as a specification. Machine-specific reports, checklists containing observations, raw logs, screenshots, and recorded fixtures stay in ignored local directories.

Tapview was used only as a technical reference. Its unconfirmed source license was not treated as permission to copy code. See [third-party notices](third-party-notices.md) and the root LICENSE.

## References

- [Windows Precision Touchpad collection](https://learn.microsoft.com/en-us/windows-hardware/design/component-guidelines/touchpad-windows-precision-touchpad-collection)
- [InjectTouchInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-injecttouchinput)
- [InitializeTouchInjection](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-initializetouchinjection)
- [GetPointerFrameInfo and message coalescing](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getpointerframeinfo)
- [TouchpadGesturesController](https://learn.microsoft.com/en-us/windows/win32/input-precisiontouchpad/touchpadgesturescontroller)
- [Three-/four-finger touchscreen interactions](https://learn.microsoft.com/en-us/troubleshoot/windows/win32/finger3or4-touch-interaction-no-longer-works)
- [Tapview Windows backend, reference only](https://github.com/JohnAZoidberg/tapview/blob/main/src/input/windows_backend.rs)
