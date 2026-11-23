use super::*;
use std::time::{Duration, Instant};

unsafe fn select_start_mode(g: &Gui, mode: TakeoverMode) {
    let index = START_MODES
        .iter()
        .position(|&(entry, _)| entry == mode)
        .unwrap();
    SendMessageW(g.item(MODE), CB_SETCURSEL, index, 0);
}

#[test]
fn escape_is_recognized_without_focus_but_not_alt_escape() {
    let key = |message, lparam| MSG {
        message,
        wParam: VK_ESCAPE as usize,
        lParam: lparam,
        ..Default::default()
    };
    assert!(is_plain_escape(&key(WM_KEYDOWN, 0)));
    // No focus window: Esc reaches the active window as WM_SYSKEYDOWN.
    assert!(is_plain_escape(&key(WM_SYSKEYDOWN, 0)));
    // Alt+Esc is the system window switch.
    assert!(!is_plain_escape(&key(WM_SYSKEYDOWN, 1 << 29)));
    assert!(!is_plain_escape(&key(WM_KEYUP, 0)));
    let mut other = key(WM_KEYDOWN, 0);
    other.wParam = VK_RETURN as usize;
    assert!(!is_plain_escape(&other));
}

#[test]
#[ignore = "shows and activates an idle native window; no device, hotkey, capture or settings changes"]
fn native_gui_focus_moves_off_disabled_controls() {
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        register("Pad2TouchGui", Some(window_proc), 0).unwrap();
        register(
            "Pad2TouchDiagnosticPanel",
            Some(panel_proc),
            CS_HREDRAW | CS_VREDRAW,
        )
        .unwrap();
        let hwnd = CreateWindowExW(
            0,
            wide("Pad2TouchGui").as_ptr(),
            wide("Pad2Touch focus regression").as_ptr(),
            WS_OVERLAPPEDWINDOW,
            40,
            40,
            100,
            100,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        );
        let mut g = Gui::new(hwnd, false).unwrap();
        g.refresh_devices();
        SendMessageW(g.item(INJECT), BM_SETCHECK, 0, 0);
        SendMessageW(g.item(PAD_GESTURES), BM_SETCHECK, 0, 0);
        SendMessageW(g.item(SCREEN_GESTURES), BM_SETCHECK, 0, 0);
        select_start_mode(&g, TakeoverMode::Toggle);
        let (start, stop) = (g.item(START), g.item(STOP));
        GUI.with(|slot| *slot.borrow_mut() = Some(g));
        GUI.with(|slot| {
            let mut slot = slot.borrow_mut();
            let g = slot.as_mut().unwrap();
            g.layout();
            g.update_controls();
        });
        ShowWindow(hwnd, SW_SHOW);
        SetActiveWindow(hwnd);
        pump_for(Duration::from_millis(100));
        assert_eq!(GetActiveWindow(), hwnd, "window could not be activated");
        // Start when a device is present, else another control the lock disables.
        let first = if IsWindowEnabled(start) != 0 {
            start
        } else {
            GetDlgItem(hwnd, MODE as i32)
        };
        SetFocus(first);
        assert_eq!(GetFocus(), first);
        // Same lock state as an enabled shortcut, without registering one.
        GUI.with(|slot| {
            let mut slot = slot.borrow_mut();
            let g = slot.as_mut().unwrap();
            g.controller.mode = TakeoverMode::Toggle;
            g.controller.enabled = true;
            g.update_controls();
        });
        assert_eq!(IsWindowEnabled(first), 0);
        assert_eq!(GetFocus(), stop, "focus was left on a disabled control");
        SendMessageW(hwnd, WM_COMMAND, STOP, 0);
        assert_eq!(IsWindowEnabled(stop), 0);
        let expected = if IsWindowEnabled(start) != 0 {
            start
        } else {
            hwnd
        };
        assert_eq!(
            GetFocus(),
            expected,
            "focus was left on the disabled Stop button"
        );
        SendMessageW(hwnd, WM_CLOSE, 0, 0);
        GUI.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

/// Phase 3 target configuration through real controls and the real
/// selection overlay, against a window of another process. No device,
/// capture session, hotkey registration or touch injection is involved.
#[test]
#[ignore = "creates native windows and a helper process; run on an interactive Windows desktop"]
fn native_gui_phase3_target_selection_without_capture() {
    use crate::{mapping::tuple, target_tests::Host};
    unsafe fn with<R>(f: impl FnOnce(&mut Gui) -> R) -> R {
        GUI.with(|slot| f(slot.borrow_mut().as_mut().unwrap()))
    }
    unsafe fn choose(hwnd: HWND, combo: usize, index: usize) {
        let item = GetDlgItem(hwnd, combo as i32);
        SendMessageW(item, CB_SETCURSEL, index, 0);
        SendMessageW(
            hwnd,
            WM_COMMAND,
            (CBN_SELCHANGE as usize) << 16 | combo,
            item as LPARAM,
        );
    }
    unsafe fn index_of(choice: Choice) -> usize {
        with(|g| g.choices.iter().position(|&c| c == choice).unwrap())
    }
    unsafe fn click(hwnd: HWND, id: usize) {
        SendMessageW(hwnd, WM_COMMAND, id, GetDlgItem(hwnd, id as i32) as LPARAM);
    }
    /// Drag inside the live overlay using client coordinates.
    unsafe fn drag(from: (i32, i32), to: (i32, i32)) {
        let overlay = FindWindowW(wide("Pad2TouchSelectionOverlay").as_ptr(), null());
        assert!(!overlay.is_null(), "selection overlay not shown");
        let pack = |(x, y): (i32, i32)| ((y as isize) << 16 | x as isize) as LPARAM;
        SendMessageW(overlay, WM_LBUTTONDOWN, 1, pack(from));
        SendMessageW(overlay, WM_MOUSEMOVE, 1, pack(to));
        SendMessageW(overlay, WM_LBUTTONUP, 0, pack(to));
        pump_for(Duration::from_millis(100));
    }
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        register("Pad2TouchGui", Some(window_proc), 0).unwrap();
        register(
            "Pad2TouchDiagnosticPanel",
            Some(panel_proc),
            CS_HREDRAW | CS_VREDRAW,
        )
        .unwrap();
        let hwnd = CreateWindowExW(
            0,
            wide("Pad2TouchGui").as_ptr(),
            wide("Pad2Touch Phase 3 GUI check").as_ptr(),
            WS_OVERLAPPEDWINDOW,
            40,
            40,
            100,
            100,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        );
        let g = Gui::new(hwnd, false).unwrap();
        // Raw-only: previews need no physical geometry.
        SendMessageW(g.item(INJECT), BM_SETCHECK, 0, 0);
        SendMessageW(g.item(PAD_GESTURES), BM_SETCHECK, 0, 0);
        SendMessageW(g.item(SCREEN_GESTURES), BM_SETCHECK, 0, 0);
        GUI.with(|slot| *slot.borrow_mut() = Some(g));
        with(|g| {
            g.layout();
            g.update_controls();
        });
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        pump_for(Duration::from_millis(100));
        with(|g| {
            assert_eq!(g.choice(), g.choices[0], "the whole screen is the default");
            assert_ne!(g.choice(), Choice::Diagnostic);
            assert!(matches!(
                g.target().unwrap(),
                MappingTarget::ScreenRectangle { .. }
            ));
            assert_eq!(IsWindowEnabled(g.item(WINDOW)), 0);
            assert_ne!(IsWindowEnabled(g.item(AREA)), 0, "Show area");
            assert_eq!(
                *g.choices.last().unwrap(),
                Choice::Diagnostic,
                "test area is listed last"
            );
        });

        let host = Host::spawn("pmv2");
        host.place(900, 120, 760, 520);

        // Application window: list, select, preview outline.
        choose(hwnd, TARGET, index_of(Choice::Window));
        let index = with(|g| {
            assert!(g.target().is_err(), "no window chosen yet");
            assert_eq!(tuple(g.rect), (0, 0, 0, 0), "no diagnostic canvas");
            assert_ne!(IsWindowEnabled(g.item(WINDOW)), 0);
            g.windows
                .iter()
                .position(|w| w.handle == host.hwnd)
                .expect("host offered in window list")
        });
        choose(hwnd, WINDOW, index);
        with(|g| {
            // Choosing a window maps its whole client area right away.
            assert_eq!(
                g.target().unwrap(),
                MappingTarget::Window { handle: host.hwnd }
            );
            assert_eq!(g.target_info(), "Whole window");
            assert_ne!(IsWindowEnabled(g.item(AREA)), 0);
        });

        // Emergency stop cancels an in-progress selection.
        click(hwnd, AREA);
        assert!(crate::overlay::active());
        SendMessageW(hwnd, WM_HOTKEY, EMERGENCY as usize, 0);
        pump_for(Duration::from_millis(100));
        with(|g| {
            assert!(!crate::overlay::active());
            assert!(g.pending_selection.is_none());
            assert!(g.window_area.is_none(), "cancel keeps the whole window");
        });

        // Select part…: drag the central half of the client area.
        click(hwnd, AREA);
        with(|g| {
            assert!(g.pending_selection.is_some());
            // Controls stay locked while the overlay is active.
            assert_eq!(IsWindowEnabled(g.item(TARGET)), 0);
        });
        let client = mapping::window_client(host.hwnd).unwrap();
        let (w, h) = mapping::size(client);
        let (w, h) = (w as i32, h as i32);
        drag((w / 4, h / 4), (w * 3 / 4 - 1, h * 3 / 4 - 1));
        with(|g| {
            let area = g.window_area.expect("window area selected");
            for (value, expected) in [
                (area.left, 0.25),
                (area.top, 0.25),
                (area.right, 0.75),
                (area.bottom, 0.75),
            ] {
                assert!((value - expected).abs() < 0.01, "{area:?}");
            }
            assert_eq!(
                g.target().unwrap(),
                MappingTarget::WindowRectangle {
                    handle: host.hwnd,
                    area
                }
            );
            assert_ne!(IsWindowEnabled(g.item(TARGET)), 0);
            assert!(g.target_info().contains('%'));
            assert!(crate::overlay::outline_visible(), "preview outline");
        });
        pump_for(Duration::from_millis(300));
        save_client_bitmap(hwnd, "diagram-window-portion.bmp");

        // The same button returns to the whole window.
        click(hwnd, AREA);
        with(|g| {
            assert!(g.window_area.is_none());
            assert_eq!(
                g.target().unwrap(),
                MappingTarget::Window { handle: host.hwnd }
            );
            assert!(
                g.last_status.starts_with("Mapped region"),
                "{}",
                g.last_status
            );
        });

        // Screen rectangle: drag in virtual-desktop coordinates.
        choose(hwnd, TARGET, index_of(Choice::ScreenPortion));
        with(|g| assert!(g.target().is_err()));
        click(hwnd, AREA);
        drag((100, 80), (419, 279));
        with(|g| {
            let screen = mapping::virtual_screen();
            let r = g.screen_rect.expect("screen area selected");
            assert_eq!(
                tuple(r),
                (
                    screen.left + 100,
                    screen.top + 80,
                    screen.left + 420,
                    screen.top + 280
                )
            );
            assert!(g.target().is_ok());
        });
        // A too-small screen selection is rejected and keeps the old one.
        click(hwnd, AREA);
        drag((10, 10), (12, 12));
        with(|g| {
            assert!(g.screen_rect.is_some());
            assert!(g.last_status.contains("at least"), "{}", g.last_status);
        });

        // The smaller preview and the pad selection overlay share the same bounds.
        choose(hwnd, PAD_FIT, 2);
        let surface = with(|g| {
            let mut client = RECT::default();
            GetClientRect(g.panel, &mut client);
            let rects: Vec<_> = g.monitors.iter().map(|m| m.rect).collect();
            let mut surface = diagram::layout(
                g.diagram_area(&client),
                mapping::virtual_screen(),
                &rects,
                g.aspect().unwrap_or(1.6),
                g.dpi,
            )
            .unwrap()
            .surface;
            let mut origin = POINT::default();
            ClientToScreen(g.panel, &mut origin);
            OffsetRect(&mut surface, origin.x, origin.y);
            surface
        });
        click(hwnd, PAD_AREA);
        with(|g| {
            let Some(Pending::Touchpad(bounds)) = g.pending_selection else {
                panic!("the touchpad selector did not open");
            };
            assert_eq!(tuple(bounds), tuple(surface));
        });
        let (width, height) = (surface.right - surface.left, surface.bottom - surface.top);
        drag((width / 4, height / 4), (width * 3 / 4, height * 3 / 4));
        with(|g| assert!(g.pad_area.is_some()));

        // A restored part waits for the first explicit window selection.
        let part = NormRect {
            left: 0.25,
            top: 0.25,
            right: 0.75,
            bottom: 0.75,
        };
        let index = with(|g| {
            g.apply_settings(&Settings {
                target: Some(SavedTarget::kind(TargetKind::Window)),
                window_area: Some(part.into()),
                ..Settings::default()
            });
            assert!(g.target().is_err(), "no window chosen yet");
            assert_eq!(g.target_info(), "Saved part");
            g.windows
                .iter()
                .position(|w| w.handle == host.hwnd)
                .unwrap()
        });
        choose(hwnd, WINDOW, index);
        with(|g| {
            assert!(g.pending_window_area.is_none());
            assert_eq!(
                g.target().unwrap(),
                MappingTarget::WindowRectangle {
                    handle: host.hwnd,
                    area: part,
                }
            );
        });

        // A closed target window is dropped from the selection.
        host.close();
        SendMessageW(
            hwnd,
            WM_COMMAND,
            (CBN_DROPDOWN as usize) << 16 | WINDOW,
            g_item(hwnd, WINDOW) as LPARAM,
        );
        with(|g| {
            assert!(g.selected_window().is_none());
            assert!(g.window_area.is_none());
            assert!(g.target().is_err());
        });
        crate::overlay::hide_outline();
        SendMessageW(hwnd, WM_CLOSE, 0, 0);
        GUI.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}
unsafe fn g_item(hwnd: HWND, id: usize) -> HWND {
    GetDlgItem(hwnd, id as i32)
}

/// Renders the panel exactly as WM_PAINT does and returns a memory DC.
unsafe fn render_panel(panel: HWND) -> (HDC, HBITMAP, HGDIOBJ) {
    let mut rc = RECT::default();
    GetClientRect(panel, &mut rc);
    let screen = GetDC(panel);
    let dc = CreateCompatibleDC(screen);
    let bitmap = CreateCompatibleBitmap(screen, rc.right, rc.bottom);
    let previous = SelectObject(dc, bitmap);
    ReleaseDC(panel, screen);
    SendMessageW(panel, WM_PRINTCLIENT, dc as WPARAM, PRF_CLIENT as LPARAM);
    (dc, bitmap, previous)
}
unsafe fn release_render((dc, bitmap, previous): (HDC, HBITMAP, HGDIOBJ)) {
    SelectObject(dc, previous);
    DeleteObject(bitmap);
    DeleteDC(dc);
}

#[test]
fn screen_area_choices_list_presets_before_custom_portions() {
    let monitor = |left: i32, primary: bool| mapping::MonitorEntry {
        rect: RECT {
            left,
            top: 0,
            right: left + 2560,
            bottom: 1440,
        },
        primary,
        device: String::new(),
    };
    // One monitor: listed once, as the whole screen.
    let single = [monitor(0, true)];
    let list = choices(single.len());
    assert_eq!(
        list,
        [
            Choice::Monitor(0),
            Choice::ScreenPortion,
            Choice::Window,
            Choice::Diagnostic
        ]
    );
    let labels: Vec<_> = list.iter().map(|&c| choice_label(c, &single)).collect();
    assert_eq!(
        labels,
        [
            "Screen · 2560×1440",
            "Portion of screen…",
            "Application window",
            "Test area"
        ]
    );
    // Several monitors: full desktop, then each monitor, main first.
    let monitors = [monitor(0, true), monitor(-2560, false)];
    let list = choices(monitors.len());
    assert_eq!(
        list,
        [
            Choice::Desktop,
            Choice::Monitor(0),
            Choice::Monitor(1),
            Choice::ScreenPortion,
            Choice::Window,
            Choice::Diagnostic
        ]
    );
    let labels: Vec<_> = list.iter().map(|&c| choice_label(c, &monitors)).collect();
    assert_eq!(labels[1], "Monitor 1 · 2560×1440 (main)");
    assert_eq!(labels[2], "Monitor 2 · 2560×1440");
    assert_eq!(choices(0)[0], Choice::Desktop);
    assert!(Choice::Window.uses_window() && !Choice::ScreenPortion.uses_window());
}

/// The Wacom-style diagram is drawn for screen targets, with the red
/// mapped region and touchpad surface where the pure layout puts them.
#[test]
#[ignore = "creates an idle native window and reads its rendering; no device, capture or injection"]
fn native_gui_mapping_diagram_draws_region_and_touchpad() {
    unsafe fn with<R>(f: impl FnOnce(&mut Gui) -> R) -> R {
        GUI.with(|slot| f(slot.borrow_mut().as_mut().unwrap()))
    }
    unsafe fn choose(hwnd: HWND, choice: Choice) {
        let index = with(|g| g.choices.iter().position(|&c| c == choice).unwrap());
        let item = GetDlgItem(hwnd, TARGET as i32);
        SendMessageW(item, CB_SETCURSEL, index, 0);
        SendMessageW(
            hwnd,
            WM_COMMAND,
            (CBN_SELCHANGE as usize) << 16 | TARGET,
            item as LPARAM,
        );
    }
    const RED: u32 = 0x002020E0;
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        register("Pad2TouchGui", Some(window_proc), 0).unwrap();
        register(
            "Pad2TouchDiagnosticPanel",
            Some(panel_proc),
            CS_HREDRAW | CS_VREDRAW,
        )
        .unwrap();
        let hwnd = CreateWindowExW(
            0,
            wide("Pad2TouchGui").as_ptr(),
            wide("Pad2Touch mapping diagram check").as_ptr(),
            WS_OVERLAPPEDWINDOW,
            40,
            40,
            100,
            100,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        );
        let g = Gui::new(hwnd, false).unwrap();
        SendMessageW(g.item(INJECT), BM_SETCHECK, 0, 0);
        SendMessageW(g.item(PAD_GESTURES), BM_SETCHECK, 0, 0);
        SendMessageW(g.item(SCREEN_GESTURES), BM_SETCHECK, 0, 0);
        let panel = g.panel;
        GUI.with(|slot| *slot.borrow_mut() = Some(g));
        with(|g| {
            g.layout();
            g.update_controls();
        });
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        pump_for(Duration::from_millis(100));
        // The decorative group must not intercept the receiver's area.
        let mut receiver_point = POINT { x: 100, y: 100 };
        assert_ne!(ClientToScreen(panel, &mut receiver_point), 0);
        assert_ne!(ScreenToClient(hwnd, &mut receiver_point), 0);
        assert_eq!(ChildWindowFromPointEx(hwnd, receiver_point, 0), panel);
        let monitors = mapping::monitors();
        assert!(!monitors.is_empty());
        with(|g| {
            assert_eq!(g.monitors.len(), monitors.len());
            assert_eq!(g.choices, choices(monitors.len()));
        });

        choose(hwnd, Choice::Monitor(0));
        // Paint an unactivated window without requesting desktop readback.
        pump_for(Duration::from_millis(250));
        InvalidateRect(panel, null(), 0);
        pump_for(Duration::from_millis(100));
        let (area, dpi) = with(|g| {
            assert_eq!(
                g.target().unwrap(),
                MappingTarget::ScreenRectangle {
                    bounds: monitors[0].rect
                }
            );
            assert!(g.target_info().starts_with(&format!(
                "{}×",
                monitors[0].rect.right - monitors[0].rect.left
            )));
            let mut rc = RECT::default();
            GetClientRect(g.panel, &mut rc);
            let area = g.diagram_area(&rc);
            assert_eq!(area.right - area.left, g.px(DIAGRAM_MAX_SIZE.0));
            assert_eq!(area.bottom - area.top, g.px(DIAGRAM_MAX_SIZE.1));
            assert!((area.left + area.right - rc.right).abs() <= 1);
            assert!(area.top > g.px(mapping::TEST_AREA_INSET));
            (area, g.dpi)
        });
        let rects: Vec<RECT> = monitors.iter().map(|m| m.rect).collect();
        let l = crate::diagram::layout(area, mapping::virtual_screen(), &rects, 1.6, dpi)
            .expect("panel large enough for the diagram");
        let region = l.screen.rect(monitors[0].rect);
        let render = render_panel(panel);
        let mid_y = (region.top + region.bottom) / 2;
        let mid_x = (l.surface.left + l.surface.right) / 2;
        assert_eq!(GetPixel(render.0, region.left, mid_y), RED, "region frame");
        assert_eq!(GetPixel(render.0, region.right - 1, mid_y), RED);
        assert_eq!(
            GetPixel(render.0, mid_x, l.surface.top),
            RED,
            "surface frame"
        );
        assert_eq!(
            GetPixel(render.0, mid_x, l.pad.top + 1),
            0x00404040,
            "pad body"
        );
        release_render(render);
        save_client_bitmap(hwnd, "diagram-monitor.bmp");

        // Repainting alone has no desktop readback side effect.
        assert_eq!(with(|g| g.thumbnail_captures), 0);
        ShowWindow(hwnd, SW_SHOW);
        use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
        let thread = GetCurrentThreadId();
        let foreground_thread = GetWindowThreadProcessId(GetForegroundWindow(), null_mut());
        let attached = foreground_thread != 0
            && foreground_thread != thread
            && AttachThreadInput(thread, foreground_thread, 1) != 0;
        SetForegroundWindow(hwnd);
        if attached {
            AttachThreadInput(thread, foreground_thread, 0);
        }
        assert_eq!(GetForegroundWindow(), hwnd);
        SetTimer(hwnd, 1, 30, None);
        pump_for(Duration::from_millis(400));
        let captured = with(|g| {
            assert!(g.thumbnail.is_some(), "foreground captures a thumbnail");
            assert_eq!(g.thumbnail_captures, 1);
            g.thumbnail_captures
        });
        pump_for(Duration::from_millis(900));
        assert_eq!(with(|g| g.thumbnail_captures), captured + 1);
        ShowWindow(hwnd, SW_MINIMIZE);
        pump_for(Duration::from_millis(1200));
        assert_eq!(with(|g| g.thumbnail_captures), captured + 1);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let mut original = RECT::default();
        GetWindowRect(hwnd, &mut original);
        choose(hwnd, Choice::ScreenPortion);
        // Restore the size the remaining pixel checks were computed for.
        SetWindowPos(
            hwnd,
            null_mut(),
            0,
            0,
            original.right - original.left,
            original.bottom - original.top,
            SWP_NOMOVE | SWP_NOZORDER,
        );
        choose(hwnd, Choice::Monitor(0));
        pump_for(Duration::from_millis(100));

        // An unconfigured target shows guidance and no mapping connectors.
        choose(hwnd, Choice::ScreenPortion);
        pump_for(Duration::from_millis(100));
        let render = render_panel(panel);
        assert_ne!(GetPixel(render.0, mid_x, l.surface.top), RED);
        release_render(render);
        save_client_bitmap(hwnd, "diagram-unselected.bmp");

        // Touchpad area: without physical geometry, injection can only
        // stretch; raw-only previews still resolve every mode.
        choose(hwnd, Choice::Monitor(0));
        with(|g| {
            g.geometry = None;
            SendMessageW(g.item(INJECT), BM_SETCHECK, 1, 0);
            for (index, ok) in [(0, false), (1, true), (2, false)] {
                SendMessageW(g.item(PAD_FIT), CB_SETCURSEL, index, 0);
                let resolved = g.resolve_target();
                assert_eq!(
                    resolved.is_ok(),
                    ok,
                    "pad fit {index}: {:?}",
                    resolved.err()
                );
                if let Ok(t) = resolved {
                    assert_eq!(t.fit, mapping::PadFit::Stretch);
                    assert_eq!(mapping::tuple(t.bounds), mapping::tuple(monitors[0].rect));
                    assert_eq!(t.input, NormRect::FULL);
                } else {
                    let error = resolved.err().unwrap();
                    assert!(error.contains(if index == 0 {
                        "Full"
                    } else {
                        "Select pad area"
                    }));
                }
            }
            SendMessageW(g.item(INJECT), BM_SETCHECK, 0, 0);
            SendMessageW(g.item(PAD_GESTURES), BM_SETCHECK, 0, 0);
            SendMessageW(g.item(SCREEN_GESTURES), BM_SETCHECK, 0, 0);
            SendMessageW(g.item(PAD_FIT), CB_SETCURSEL, 2, 0);
            g.select_pad_area().unwrap();
        });
        let picker = FindWindowW(wide("Pad2TouchSelectionOverlay").as_ptr(), null());
        assert!(!picker.is_null());
        let mut picker_bounds = RECT::default();
        GetClientRect(picker, &mut picker_bounds);
        let point = |x: i32, y: i32| ((y << 16) | x) as LPARAM;
        SendMessageW(
            picker,
            WM_LBUTTONDOWN,
            1,
            point(picker_bounds.right / 4, picker_bounds.bottom / 4),
        );
        SendMessageW(
            picker,
            WM_LBUTTONUP,
            0,
            point(picker_bounds.right * 3 / 4, picker_bounds.bottom * 3 / 4),
        );
        pump_for(Duration::from_millis(100));
        with(|g| {
            let target = g.resolve_target().unwrap();
            assert_eq!(
                mapping::tuple(target.bounds),
                mapping::tuple(monitors[0].rect)
            );
            assert!((target.input.left - 0.25).abs() < 0.02);
            assert!((target.input.right - 0.75).abs() < 0.02);
            SendMessageW(g.item(PAD_FIT), CB_SETCURSEL, 0, 0);
        });

        // Back to the diagnostic receiver: no diagram.
        choose(hwnd, Choice::Diagnostic);
        pump_for(Duration::from_millis(100));
        let render = render_panel(panel);
        assert_ne!(GetPixel(render.0, mid_x, l.pad.top + 1), 0x00404040);
        release_render(render);

        SendMessageW(hwnd, WM_CLOSE, 0, 0);
        GUI.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

unsafe fn assert_live_layout(hwnd: HWND) {
    GUI.with(|slot| {
        let slot = slot.borrow();
        let g = slot.as_ref().unwrap();
        // Without DCX_CLIPCHILDREN: some sample points lie inside group
        // boxes, which are child windows that paint the same background.
        let dc = GetDCEx(hwnd, null_mut(), DCX_CACHE);
        let background = GetSysColor(COLOR_BTNFACE);
        for (x, y) in [
            (228, 70),
            (225, 83),
            (250, 214),
            (250, 318),
            (285, 378),
            (260, 230),
            (600, 12),
        ] {
            assert_eq!(
                GetPixel(dc, g.px(x), g.px(y)),
                background,
                "live dialog background differs at ({x}, {y})"
            );
        }
        for y in g.px(SETTINGS_BOTTOM)..g.px(FOOTER_TOP) {
            assert_eq!(
                GetPixel(dc, g.px(400), y),
                background,
                "footer gap at y={y}"
            );
        }
        ReleaseDC(hwnd, dc);
        if g.rect.right > g.rect.left {
            let panel_dc = GetDC(g.panel);
            let mut canvas = POINT {
                x: g.rect.left,
                y: g.rect.top,
            };
            ScreenToClient(g.panel, &mut canvas);
            assert_eq!(
                GetPixel(panel_dc, canvas.x, canvas.y + 1),
                0x00808080,
                "canvas border moved or was not repainted"
            );
            ReleaseDC(g.panel, panel_dc);
            let mut origin = POINT::default();
            ClientToScreen(g.panel, &mut origin);
            assert!(g.rect.left - origin.x >= g.px(16));
            assert!(g.rect.top - origin.y >= g.px(mapping::TEST_AREA_INSET));
            if let Some(aspect) = g.aspect() {
                let target = g.resolve_target().unwrap();
                assert_eq!(target.input, NormRect::FULL, "Test area uses the whole pad");
                assert_eq!(mapping::tuple(g.rect), mapping::tuple(target.bounds));
                let (w, h) = mapping::size(g.rect);
                assert!(
                    (w as f64 - h as f64 * aspect).abs() <= aspect.max(1.0),
                    "Test area canvas {w}x{h} must match the physical pad ratio {aspect}"
                );
            } else {
                assert_eq!(g.rect.top - origin.y, g.px(mapping::TEST_AREA_INSET));
            }
        }
        let mut client = RECT::default();
        GetClientRect(hwnd, &mut client);
        for c in &g.controls {
            let mut bounds = RECT::default();
            GetWindowRect(c.hwnd, &mut bounds);
            let mut top = POINT {
                x: bounds.left,
                y: bounds.top,
            };
            let mut bottom = POINT {
                x: bounds.right,
                y: bounds.bottom,
            };
            ScreenToClient(hwnd, &mut top);
            ScreenToClient(hwnd, &mut bottom);
            assert!(
                top.x >= 0 && top.y >= 0 && bottom.x <= client.right && bottom.y <= client.bottom,
                "control clipped at {}/{}, {}/{}",
                top.x,
                top.y,
                bottom.x,
                bottom.y
            );
        }
    });
}

#[test]
#[ignore = "shows an idle native window briefly and checks actual displayed pixels after moving/minimizing/restoring; no capture or settings changes"]
fn native_gui_fixed_size_repaints_background_and_canvas() {
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        register("Pad2TouchGui", Some(window_proc), 0).unwrap();
        register(
            "Pad2TouchDiagnosticPanel",
            Some(panel_proc),
            CS_HREDRAW | CS_VREDRAW,
        )
        .unwrap();
        let hwnd = CreateWindowExW(
            0,
            wide("Pad2TouchGui").as_ptr(),
            wide("Pad2Touch fixed-size regression").as_ptr(),
            WS_OVERLAPPEDWINDOW,
            20,
            20,
            100,
            100,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        );
        let mut gui = Gui::new(hwnd, false).unwrap();
        assert!(gui.item(LOG_PATH).is_null());
        assert!(gui.checked(FEEDBACK), "touch feedback defaults to enabled");
        let gesture_default = crate::gesture_backend::overrides_enabled_by_default();
        assert_eq!(gui.checked(PAD_GESTURES), gesture_default);
        assert_eq!(gui.checked(SCREEN_GESTURES), gesture_default);
        gui.refresh_devices();
        let dpi = gui.dpi;
        let expected = (gui.px(DEFAULT_CLIENT.0), gui.px(DEFAULT_CLIENT.1));
        GUI.with(|slot| *slot.borrow_mut() = Some(gui));
        GUI.with(|slot| {
            let mut slot = slot.borrow_mut();
            let g = slot.as_mut().unwrap();
            g.layout();
            let shown = |id| GetWindowLongPtrW(g.item(id), GWL_STYLE) as u32 & WS_VISIBLE != 0;
            let bounds = |id| child_rect(hwnd, g.item(id));
            let enabled = |id| IsWindowEnabled(g.item(id)) != 0;
            let combos = [DEVICE, MODE, KEY, TARGET, WINDOW, PAD_FIT];
            let positions = || combos.map(|id| mapping::tuple(bounds(id)));
            g.update_controls();
            for id in combos {
                assert!(shown(id), "dropdown {id} is hidden in manual mode");
                let style = GetWindowLongPtrW(g.item(id), GWL_STYLE) as i32;
                assert_eq!(style & (CBS_OWNERDRAWFIXED | CBS_OWNERDRAWVARIABLE), 0);
            }
            assert!(!enabled(KEY) && !enabled(WINDOW) && !enabled(PAD_AREA));
            assert_eq!(bounds(INJECT).left, bounds(FEEDBACK).left);
            let start_top = bounds(START).top;
            let default_positions = positions();
            for (label, combo) in [
                (SCREEN_AREA_LABEL, TARGET),
                (WINDOW_LABEL, WINDOW),
                (PAD_AREA_LABEL, PAD_FIT),
            ] {
                assert_eq!(bounds(label).left, bounds(INJECT).left);
                assert_eq!(bounds(label).top, bounds(combo).top);
                assert_eq!(bounds(combo).left, bounds(MODE).left);
                assert_eq!(bounds(combo).right, bounds(MODE).right);
            }
            for (above, below, gap) in [
                (TARGET, WINDOW, 3),
                (WINDOW, AREA, 3),
                (AREA, PAD_AREA_LABEL, 6),
                (PAD_FIT, PAD_AREA, 3),
            ] {
                assert!(bounds(below).top - bounds(above).bottom >= g.px(gap));
            }
            assert!(bounds(PAD_AREA).bottom < bounds(MAPPING_GROUP).bottom);
            let preview = bounds(PREVIEW_GROUP);
            let start = bounds(START);
            let stop = bounds(STOP);
            assert_eq!(preview.top, g.px(SIDEBAR_TOP));
            assert_eq!(preview.bottom, bounds(MAPPING_GROUP).bottom);
            assert!(child_rect(hwnd, g.panel).bottom < preview.bottom);
            let hint = bounds(STOP_HINT);
            assert!(preview.bottom < hint.top);
            assert!(bounds(STATUS).right < bounds(STATUS_DETAIL).left);
            assert!(bounds(STATUS_DETAIL).right < hint.left && hint.right < start.left);
            assert!(((hint.top + hint.bottom) / 2 - (start.top + start.bottom) / 2).abs() <= 1);
            assert!(preview.left < start.left && start.right < stop.left);
            assert_eq!(start.top, stop.top);
            assert_eq!(preview.right, stop.right);
            assert!(stop.bottom < expected.1);

            SendMessageW(g.item(MODE), CB_SETCURSEL, 1, 0);
            g.update_controls();
            assert_eq!(g.selected_mode(), TakeoverMode::Hold);
            assert!(shown(KEY) && !enabled(KEY));
            assert_eq!(positions(), default_positions);

            SendMessageW(g.item(MODE), CB_SETCURSEL, 2, 0);
            g.select_choice(Choice::Window);
            SendMessageW(g.item(PAD_FIT), CB_SETCURSEL, 2, 0);
            g.update_controls();
            assert_eq!(g.selected_mode(), TakeoverMode::Toggle);
            assert!(shown(KEY) && enabled(KEY));
            assert!(shown(WINDOW) && enabled(WINDOW));
            assert!(shown(PAD_AREA) && enabled(PAD_AREA));
            assert_eq!(positions(), default_positions);
            assert_eq!(bounds(START).top, start_top);

            select_start_mode(g, TakeoverMode::Manual);
            SendMessageW(g.item(PAD_FIT), CB_SETCURSEL, 0, 0);
            let test_area = g.choices.iter().position(|&c| c == Choice::Diagnostic);
            SendMessageW(g.item(TARGET), CB_SETCURSEL, test_area.unwrap(), 0);
            if g.geometry.is_none() {
                SendMessageW(g.item(PAD_FIT), CB_SETCURSEL, 1, 0);
            }
            g.update_controls();
            for id in combos {
                assert!(shown(id), "dropdown {id} is hidden for the test area");
            }
            assert!(!enabled(WINDOW) && !enabled(AREA) && !enabled(PAD_AREA));
            assert_eq!(positions(), default_positions);
            assert_eq!(bounds(START).top, start_top);
            g.layout();
            assert!(g.rect.right > g.rect.left, "Test area retains its canvas");
        });
        // Even when created with a sizable frame, the window becomes fixed.
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        assert_eq!(
            style & (WS_THICKFRAME | WS_MAXIMIZEBOX),
            0,
            "window is resizable"
        );
        assert_ne!(
            style & WS_CLIPCHILDREN,
            0,
            "background paints under controls"
        );
        let client = || {
            let mut rc = RECT::default();
            GetClientRect(hwnd, &mut rc);
            (rc.right, rc.bottom)
        };
        assert_eq!(client(), expected, "client size");
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            20,
            20,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE,
        );
        pump_for(Duration::from_millis(150));
        assert_live_layout(hwnd);
        save_client_preview(hwnd);
        let mut before = RECT::default();
        GetWindowRect(hwnd, &mut before);
        SetWindowPos(
            hwnd,
            null_mut(),
            before.left + 70,
            before.top + 35,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        pump_for(Duration::from_millis(100));
        assert_live_layout(hwnd);
        ShowWindow(hwnd, SW_MINIMIZE);
        pump_for(Duration::from_millis(100));
        ShowWindow(hwnd, SW_RESTORE);
        pump_for(Duration::from_millis(150));
        assert_eq!(client(), expected, "client size after restore");
        assert_live_layout(hwnd);
        save_client_preview(hwnd);
        SendMessageW(hwnd, WM_CLOSE, 0, 0);
        GUI.with(|slot| {
            slot.borrow_mut().take();
        });
        println!("native_fixed_size=passed client={expected:?} dpi={dpi}");
    }
}

#[test]
#[ignore = "run on an idle interactive desktop; temporarily blocks gestures and checks the startup cursor during raw capture"]
fn native_gui_background_guard_session_restores() {
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        register("Pad2TouchGui", Some(window_proc), 0).unwrap();
        register(
            "Pad2TouchDiagnosticPanel",
            Some(panel_proc),
            CS_HREDRAW | CS_VREDRAW,
        )
        .unwrap();
        let hwnd = CreateWindowExW(
            0,
            wide("Pad2TouchGui").as_ptr(),
            wide("Pad2Touch background integration check").as_ptr(),
            WS_OVERLAPPEDWINDOW,
            80,
            80,
            1120,
            760,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        );
        let mut g = Gui::new(hwnd, false).unwrap();
        g.refresh_devices();
        SendMessageW(g.item(INJECT), BM_SETCHECK, 0, 0);
        SendMessageW(g.item(PAD_GESTURES), BM_SETCHECK, 1, 0);
        SendMessageW(g.item(SCREEN_GESTURES), BM_SETCHECK, 1, 0);
        GUI.with(|slot| *slot.borrow_mut() = Some(g));
        GUI.with(|slot| slot.borrow_mut().as_mut().unwrap().layout());
        SetTimer(hwnd, 1, 30, None);
        let busy_cursor = LoadCursorW(null_mut(), IDC_APPSTARTING);
        assert!(!busy_cursor.is_null());
        let mut cursor = CURSORINFO {
            cbSize: std::mem::size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        if GetCursorInfo(&mut cursor) != 0 && cursor.flags & CURSOR_SHOWING != 0 {
            assert_ne!(cursor.hCursor, busy_cursor, "desktop was already busy");
        }
        SendMessageW(hwnd, WM_COMMAND, START, 0);
        let start = Instant::now();
        let mut visible_samples = 0;
        let mut busy_samples = 0;
        while start.elapsed() < Duration::from_secs(4) {
            pump_for(Duration::from_millis(5));
            if GetCursorInfo(&mut cursor) != 0 && cursor.flags & CURSOR_SHOWING != 0 {
                visible_samples += 1;
                busy_samples += usize::from(cursor.hCursor == busy_cursor);
            }
        }
        GUI.with(|slot| {
            let slot = slot.borrow();
            let g = slot.as_ref().unwrap();
            assert!(g.session.is_some(), "{}", g.last_status);
            let s = g.shared.lock().unwrap();
            assert!(
                s.guard_status.contains("ACTIVE: touchpad + touchscreen"),
                "{} / {}",
                s.guard_status,
                s.error
            );
        });
        SendMessageW(hwnd, WM_COMMAND, STOP, 0);
        pump_for(Duration::from_secs(4));
        GUI.with(|slot| {
            let slot = slot.borrow();
            let g = slot.as_ref().unwrap();
            assert!(g.session.is_none(), "guard did not finish");
            assert!(!g.restart_blocked, "{}", g.last_status);
            let s = g.shared.lock().unwrap();
            assert_eq!(s.guard_status, "Windows gestures restored");
            assert!(!s.recovery_pending);
            assert!(s.error.is_empty(), "{}", s.error);
        });
        SendMessageW(hwnd, WM_CLOSE, 0, 0);
        GUI.with(|slot| {
            slot.borrow_mut().take();
        });
        assert_eq!(
            busy_samples, 0,
            "startup cursor in {busy_samples}/{visible_samples} samples"
        );
        if visible_samples == 0 {
            println!("startup_cursor=unobserved (desktop cursor was hidden)");
        } else {
            println!("startup_cursor_samples={busy_samples}/{visible_samples}");
        }
    }
}

unsafe fn save_client_preview(hwnd: HWND) {
    save_client_bitmap(hwnd, "gui-preview.bmp");
}
unsafe fn save_client_bitmap(hwnd: HWND, filename: &str) {
    let mut rc = RECT::default();
    GetClientRect(hwnd, &mut rc);
    let screen = GetDC(hwnd);
    let dc = CreateCompatibleDC(screen);
    let bitmap = CreateCompatibleBitmap(screen, rc.right, rc.bottom);
    let previous = SelectObject(dc, bitmap);
    SendMessageW(hwnd, WM_PRINTCLIENT, dc as WPARAM, 0);
    let mut child = GetWindow(hwnd, GW_CHILD);
    while !child.is_null() {
        let mut child_rect = RECT::default();
        GetWindowRect(child, &mut child_rect);
        let mut at = POINT {
            x: child_rect.left,
            y: child_rect.top,
        };
        ScreenToClient(hwnd, &mut at);
        let state = SaveDC(dc);
        SetViewportOrgEx(dc, at.x, at.y, null_mut());
        SendMessageW(
            child,
            WM_PRINT,
            dc as WPARAM,
            (PRF_CLIENT | PRF_CHILDREN | PRF_ERASEBKGND) as LPARAM,
        );
        RestoreDC(dc, state);
        child = GetWindow(child, GW_HWNDNEXT);
    }
    SelectObject(dc, previous);
    let mut info = BITMAPINFO::default();
    info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    info.bmiHeader.biWidth = rc.right;
    info.bmiHeader.biHeight = -rc.bottom;
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB;
    let mut pixels = vec![0u8; (rc.right * rc.bottom * 4) as usize];
    assert_ne!(
        GetDIBits(
            dc,
            bitmap,
            0,
            rc.bottom as u32,
            pixels.as_mut_ptr().cast(),
            &mut info,
            DIB_RGB_COLORS
        ),
        0
    );
    let mut bmp = vec![0u8; 54];
    bmp[0..2].copy_from_slice(b"BM");
    bmp[2..6].copy_from_slice(&((54 + pixels.len()) as u32).to_le_bytes());
    bmp[10..14].copy_from_slice(&54u32.to_le_bytes());
    bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
    bmp[18..22].copy_from_slice(&rc.right.to_le_bytes());
    bmp[22..26].copy_from_slice(&(-rc.bottom).to_le_bytes());
    bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
    bmp[28..30].copy_from_slice(&32u16.to_le_bytes());
    bmp.extend_from_slice(&pixels);
    DeleteObject(bitmap);
    DeleteDC(dc);
    ReleaseDC(hwnd, screen);

    let directory = std::path::Path::new("artifacts/screenshots");
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(directory.join(filename), bmp).unwrap();
}

unsafe fn pump_for(duration: Duration) {
    let start = Instant::now();
    while start.elapsed() < duration {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
            if msg.message != WM_QUIT {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "creates native controls and raw-only capture sessions; run alone on an interactive Windows desktop"]
fn native_gui_sessions_restart_and_hotkey_messages_stop_cleanly() {
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        register("Pad2TouchGui", Some(window_proc), 0).unwrap();
        register(
            "Pad2TouchDiagnosticPanel",
            Some(panel_proc),
            CS_HREDRAW | CS_VREDRAW,
        )
        .unwrap();
        let hwnd = CreateWindowExW(
            0,
            wide("Pad2TouchGui").as_ptr(),
            wide("Pad2Touch GUI integration check").as_ptr(),
            WS_OVERLAPPEDWINDOW,
            80,
            80,
            1280,
            860,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        );
        assert!(!hwnd.is_null());
        let mut g = Gui::new(hwnd, true).unwrap();
        assert!(!g.item(LOG_PATH).is_null());
        g.refresh_devices();
        assert!(!g.devices.is_empty(), "requires a local Precision Touchpad");
        SendMessageW(g.item(INJECT), BM_SETCHECK, 0, 0);
        SendMessageW(g.item(PAD_GESTURES), BM_SETCHECK, 0, 0);
        SendMessageW(g.item(SCREEN_GESTURES), BM_SETCHECK, 0, 0);
        GUI.with(|slot| *slot.borrow_mut() = Some(g));
        GUI.with(|slot| {
            let mut slot = slot.borrow_mut();
            let g = slot.as_mut().unwrap();
            g.layout();
            g.update_controls();
        });
        SetTimer(hwnd, 1, 30, None);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        pump_for(Duration::from_millis(100));
        save_client_preview(hwnd);
        // Real WM_COMMAND dispatch, actual Raw Input worker/window and logger.
        // These checks never initialize or submit native touch injection.
        for _ in 0..2 {
            SendMessageW(hwnd, WM_COMMAND, START, 0);
            pump_for(Duration::from_millis(250));
            GUI.with(|slot| {
                let slot = slot.borrow();
                let g = slot.as_ref().unwrap();
                assert!(g.session.is_some());
                assert!(!g.session.as_ref().unwrap().worker_done());
                assert_eq!(IsWindowEnabled(g.item(DEVICE)), 0);
            });
            SendMessageW(hwnd, WM_COMMAND, STOP, 0);
            pump_for(Duration::from_millis(650));
            GUI.with(|slot| {
                let slot = slot.borrow();
                let g = slot.as_ref().unwrap();
                assert!(g.session.is_none());
                assert!(!g.controller.enabled);
                assert!(!g.restart_blocked);
                let mut path = vec![0u16; 1024];
                let len = GetWindowTextW(g.log_path, path.as_mut_ptr(), path.len() as i32);
                let label = String::from_utf16_lossy(&path[..len as usize]);
                let out = label.strip_prefix("Logs: ").unwrap();
                let log = std::fs::read_to_string(PathBuf::from(out).join("events.log")).unwrap();
                assert!(log.contains("REGISTER_RAW_INPUT success"));
                assert!(log.contains("WORKER_STOP active_contacts=0 cleanup_pending=false"));
                assert!(log.contains("SUMMARY"));
                assert!(PathBuf::from(out).join("summary.txt").exists());
            });
        }
        // Reserve a chord to verify that a registration conflict cannot arm.
        let key = VK_F8 as u32;
        if RegisterHotKey(hwnd, 99, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, key) != 0 {
            GUI.with(|slot| {
                let mut slot = slot.borrow_mut();
                let g = slot.as_mut().unwrap();
                select_start_mode(g, TakeoverMode::Toggle);
            });
            SendMessageW(hwnd, WM_COMMAND, START, 0);
            GUI.with(|slot| assert!(!slot.borrow().as_ref().unwrap().controller.enabled));
            UnregisterHotKey(hwnd, 99);
        }
        GUI.with(|slot| {
            let mut slot = slot.borrow_mut();
            let g = slot.as_mut().unwrap();
            select_start_mode(g, TakeoverMode::Toggle);
        });
        SendMessageW(hwnd, WM_COMMAND, START, 0);
        GUI.with(|slot| {
            let slot = slot.borrow();
            let g = slot.as_ref().unwrap();
            assert!(g.controller.enabled, "Toggle failed: {}", g.last_status);
        });
        SendMessageW(hwnd, WM_HOTKEY, HOTKEY as usize, 0);
        pump_for(Duration::from_millis(250));
        GUI.with(|slot| assert!(slot.borrow().as_ref().unwrap().session.is_some()));
        SendMessageW(hwnd, WM_HOTKEY, HOTKEY as usize, 0);
        pump_for(Duration::from_millis(650));
        GUI.with(|slot| {
            let slot = slot.borrow();
            let g = slot.as_ref().unwrap();
            assert!(g.session.is_none());
            assert!(g.controller.enabled);
        });
        SendMessageW(hwnd, WM_HOTKEY, EMERGENCY as usize, 0);
        GUI.with(|slot| {
            let slot = slot.borrow();
            let g = slot.as_ref().unwrap();
            assert!(!g.controller.enabled);
            assert!(!g.hotkey_registered);
        });
        // A stale Toggle notification cannot start an Alt Hold session.
        GUI.with(|slot| {
            let mut slot = slot.borrow_mut();
            let g = slot.as_mut().unwrap();
            select_start_mode(g, TakeoverMode::Hold);
            SendMessageW(g.item(PAD_GESTURES), BM_SETCHECK, 1, 0);
            SendMessageW(g.item(SCREEN_GESTURES), BM_SETCHECK, 1, 0);
        });
        let send_alt = |flags| {
            let event = INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VK_LMENU,
                        dwFlags: flags,
                        ..Default::default()
                    },
                },
            };
            assert_eq!(SendInput(1, &event, size_of::<INPUT>() as i32), 1);
        };
        struct ReleaseAlt<F: Fn(u32)>(F);
        impl<F: Fn(u32)> Drop for ReleaseAlt<F> {
            fn drop(&mut self) {
                (self.0)(KEYEVENTF_KEYUP);
            }
        }
        assert!(!Gui::alt_down());
        send_alt(0);
        let initial_alt = ReleaseAlt(send_alt);
        pump_for(Duration::from_millis(60));
        assert!(Gui::alt_down());
        SendMessageW(hwnd, WM_COMMAND, START, 0);
        SendMessageW(hwnd, WM_HOTKEY, HOTKEY as usize, 0);
        pump_for(Duration::from_millis(100));
        GUI.with(|slot| assert!(slot.borrow().as_ref().unwrap().session.is_none()));
        drop(initial_alt);
        pump_for(Duration::from_millis(80));
        for _ in 0..2 {
            send_alt(0);
            let alt = ReleaseAlt(send_alt);
            pump_for(Duration::from_secs(2));
            GUI.with(|slot| {
                let slot = slot.borrow();
                let g = slot.as_ref().unwrap();
                assert!(g.prewarmed.is_some());
                assert!(g.session.is_some(), "{}", g.last_status);
                assert!(g
                    .shared
                    .lock()
                    .unwrap()
                    .guard_status
                    .contains("ACTIVE: touchpad + touchscreen"));
            });
            drop(alt);
            pump_for(Duration::from_millis(1500));
            GUI.with(|slot| {
                let slot = slot.borrow();
                let g = slot.as_ref().unwrap();
                assert!(g.session.is_none(), "{}", g.last_status);
                assert!(g.controller.enabled);
                assert!(!g.restart_blocked);
                assert_eq!(
                    g.shared.lock().unwrap().guard_status,
                    "Windows gestures restored"
                );
            });
        }
        SendMessageW(hwnd, WM_HOTKEY, EMERGENCY as usize, 0);
        // Closing an active session must keep the receiver alive for drain.
        GUI.with(|slot| {
            let mut slot = slot.borrow_mut();
            let g = slot.as_mut().unwrap();
            select_start_mode(g, TakeoverMode::Manual);
            SendMessageW(g.item(PAD_GESTURES), BM_SETCHECK, 0, 0);
            SendMessageW(g.item(SCREEN_GESTURES), BM_SETCHECK, 0, 0);
        });
        SendMessageW(hwnd, WM_COMMAND, START, 0);
        pump_for(Duration::from_millis(250));
        GUI.with(|slot| assert!(slot.borrow().as_ref().unwrap().session.is_some()));
        SendMessageW(hwnd, WM_CLOSE, 0, 0);
        assert_ne!(IsWindow(hwnd), 0);
        pump_for(Duration::from_millis(650));
        assert_eq!(IsWindow(hwnd), 0);
        GUI.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}
