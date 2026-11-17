//! Target selection boundary for Phase 3. Unimplemented targets fail explicitly.
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::ClientToScreen,
    UI::{HiDpi::GetDpiForWindow, WindowsAndMessaging::*},
};

#[derive(Clone, Copy, Default)]
pub enum MappingTarget {
    #[default]
    Diagnostics,
    ScreenRectangle {
        bounds: RECT,
    },
    Window {
        handle: usize,
    },
    WindowRectangle {
        handle: usize,
        client_bounds: RECT,
    },
}

#[derive(Clone, Copy)]
pub struct ResolvedTarget {
    pub window: usize,
    pub bounds: RECT,
}

/// Fit a physical aspect into available pixels; one-pixel rounding is inevitable.
pub fn fit_aspect(area: RECT, aspect: f64) -> Result<RECT, String> {
    let w = area.right as i64 - area.left as i64;
    let h = area.bottom as i64 - area.top as i64;
    if !aspect.is_finite()
        || aspect <= 0.0
        || !(2..=i32::MAX as i64).contains(&w)
        || !(2..=i32::MAX as i64).contains(&h)
    {
        return Err("invalid physical aspect or available rectangle".into());
    }
    let (width, height) = if w as f64 / h as f64 > aspect {
        ((h as f64 * aspect).round() as i64, h)
    } else {
        (w, (w as f64 / aspect).round() as i64)
    };
    if width < 2 || height < 2 || width > w || height > h {
        return Err("physical aspect cannot fit the available pixels".into());
    }
    let left = area.left + ((w - width) / 2) as i32;
    let top = area.top + ((h - height) / 2) as i32;
    Ok(RECT {
        left,
        top,
        right: left + width as i32,
        bottom: top + height as i32,
    })
}

impl MappingTarget {
    pub fn resolve_with_aspect(self, window: HWND, aspect: f64) -> Result<ResolvedTarget, String> {
        let mut target = self.resolve(window)?;
        target.bounds = fit_aspect(target.bounds, aspect)?;
        Ok(target)
    }
    pub fn resolve(self, diagnostic_window: HWND) -> Result<ResolvedTarget, String> {
        match self {
            Self::Diagnostics => unsafe {
                let dpi = GetDpiForWindow(diagnostic_window).max(96) as i32;
                let px = |n: i32| n * dpi / 96;
                let mut client = RECT::default();
                let mut origin = POINT::default();
                if GetClientRect(diagnostic_window, &mut client) == 0
                    || ClientToScreen(diagnostic_window, &mut origin) == 0
                {
                    return Err(crate::hid::winerr("resolve GUI diagnostic rectangle"));
                }
                if client.right < px(280) || client.bottom < px(320) {
                    return Err(
                        "The touch workspace is too small. Enlarge the window before capturing."
                            .into(),
                    );
                }
                Ok(ResolvedTarget {
                    window: diagnostic_window as usize,
                    bounds: RECT {
                        left: origin.x + px(16),
                        top: origin.y + px(190),
                        right: origin.x + client.right - px(16),
                        bottom: origin.y + client.bottom - px(20),
                    },
                })
            },
            Self::ScreenRectangle { bounds } => Err(format!(
                "Screen rectangle mapping is planned ({}, {}, {}, {})。",
                bounds.left, bounds.top, bounds.right, bounds.bottom
            )),
            Self::Window { handle } => Err(format!(
                "Application window mapping is planned (0x{handle:X})。"
            )),
            Self::WindowRectangle {
                handle,
                client_bounds,
            } => Err(format!(
                "Window rectangle mapping is planned (0x{handle:X}, {}, {}, {}, {})。",
                client_bounds.left, client_bounds.top, client_bounds.right, client_bounds.bottom
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn physical_shape_is_centered_inside_wide_tall_and_negative_screen_bounds() {
        for area in [
            RECT {
                left: -1600,
                top: 30,
                right: -600,
                bottom: 330,
            },
            RECT {
                left: 10,
                top: -800,
                right: 310,
                bottom: 100,
            },
            RECT {
                left: 0,
                top: 0,
                right: 1001,
                bottom: 777,
            },
        ] {
            for ratio in [1.6003, 0.6, 2.4] {
                let r = fit_aspect(area, ratio).unwrap();
                assert!(
                    r.left >= area.left
                        && r.right <= area.right
                        && r.top >= area.top
                        && r.bottom <= area.bottom
                );
                assert!(
                    ((r.right - r.left) as f64 - (r.bottom - r.top) as f64 * ratio).abs()
                        <= ratio.max(1.0)
                );
                assert!(((r.left - area.left) - (area.right - r.right)).abs() <= 1);
                assert!(((r.top - area.top) - (area.bottom - r.bottom)).abs() <= 1);
            }
        }
        for ratio in [f64::NAN, f64::INFINITY, 0.0, -1.0, 1e-100, 1e100] {
            assert!(fit_aspect(
                RECT {
                    left: 0,
                    top: 0,
                    right: 100,
                    bottom: 100
                },
                ratio
            )
            .is_err());
        }
    }
    #[test]
    fn reserved_targets_never_fall_back_to_diagnostic_injection() {
        for target in [
            MappingTarget::ScreenRectangle {
                bounds: RECT::default(),
            },
            MappingTarget::Window { handle: 1 },
            MappingTarget::WindowRectangle {
                handle: 1,
                client_bounds: RECT::default(),
            },
        ] {
            assert!(target.resolve(std::ptr::null_mut()).is_err());
        }
    }
}
