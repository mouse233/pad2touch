# Architecture

PadBridge is a Windows user-mode bridge from Precision Touchpad HID reports to native touch injection. The current MVP keeps a fixed receiver rectangle and a minimal diagnostic window. Application-window targeting, persistent configuration, a full GUI, and release-level recovery remain outside this implementation.

## Modules

| Module | Responsibility |
|---|---|
| `main.rs` | Validate command-line options and dispatch capture or offline log verification. |
| `hid.rs` | Discover Raw Input collections, retrieve HidP capabilities and values, and validate aligned RAWHID buffers. |
| `decode.rs` | Interpret fields, assemble complete reports, and map absolute coordinates without Windows API dependencies. |
| `touch.rs` | Plan complete contact frames, assign stable slots, commit successful state, retry, and cancel through an abstract transport. |
| `injection.rs` | Implement the Win32 transport using hit testing, POINTER_TOUCH_INFO arrays, and InjectTouchInput. |
| `scenarios.rs` | Generate deterministic logical scenarios for offline tests and the explicitly requested native API exercise. |
| `experiment.rs` | Own the input worker, receiver window, diagnostics, contact trails, and coordinated shutdown. |
| `verify.rs` | Audit batch completeness, contact lifecycles, and received coordinates without injecting input. |
| `logging.rs` | Write diagnostics through a bounded queue and count dropped records. |

Raw Input processing and injection run serially on one worker thread. The receiver processes pointer messages on a separate thread. Painting copies a bounded display snapshot and releases the statistics lock before GDI calls. Log writing runs on its own thread; queue overflow is reported rather than treated as complete evidence.

## Input discovery and parsing

Discovery selects Digitizers / Touch Pad collections (`0D:05`). The selected raw device handle identifies the source throughout capture. `RIDI_PREPARSEDDATA` is cached, and HidP supplies report IDs, data indices, link collections, button capabilities, and logical coordinate ranges. Device-specific bit offsets are not hardcoded.

RAWHID buffers use aligned storage and checked sizes. Every report in `dwSizeHid * dwCount` is processed. Foreground WM_INPUT messages still pass through DefWindowProc for cleanup.

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

Normal DOWN/UPDATE submissions must hit the dedicated receiver according to WindowFromPoint. Cleanup bypasses this check so an existing sequence can still be ended after occlusion. Window movement, minimization, device removal, and display changes trigger cancellation or stopping; automatic retargeting and reconnection are not implemented.

## Receiver and shutdown

The receiver only counts messages confirmed as PT_TOUCH by GetPointerInfo and also calls GetPointerTouchInfo. Each system pointer ID has an independent trail, color, and active position. Two received contacts drive a distance ratio and a small scale demonstration; this is a receiver diagnostic rather than proof of third-party gesture support.

Esc, WM_CLOSE, Ctrl+C, and duration expiry request worker shutdown. The worker attempts cancellation before publishing completion. The receiver then continues processing messages for approximately 300 ms before closing, allowing final UP events to arrive. Summaries include contacts remaining on both sides, cleanup failures, and log loss.

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
- [Tapview Windows backend, reference only](https://github.com/JohnAZoidberg/tapview/blob/main/src/input/windows_backend.rs)
