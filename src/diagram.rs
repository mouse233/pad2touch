//! Wacom-style mapping diagram: monitors on top (with a live desktop
//! thumbnail), the touchpad below at its physical aspect, and red connectors
//! from the touchpad surface to the mapped screen region.
//!
//! Reading the composed desktop back from DWM takes about two frames
//! (~33 ms, ~7 ms CPU), so the thumbnail is captured only when the caller
//! asks for it; ordinary repaints reuse it, scaled if the panel resized.
//!
//! Layout is pure and unit-tested; painting uses plain GDI on the caller's
//! back buffer. The thumbnail is only drawn on screen, never logged or saved.
use crate::{
    experiment::wide,
    hid::winerr,
    mapping::{fit_aspect, NormRect},
};
use std::ptr::null_mut;
use windows_sys::Win32::{Foundation::*, Graphics::Gdi::*};

const RED: u32 = 0x002020E0;
const BEZEL: u32 = 0x00383838;
const STAND: u32 = 0x00A0A0A0;
const PAD_BODY: u32 = 0x00404040;
const PAD_SURFACE: u32 = 0x00585858;
const PAD_UNUSED: u32 = 0x00484848;
const SELECTED: u32 = 0x00C0C0C0;
const EMPTY_SCREEN: u32 = 0x00806040;
const CONTACT_COLORS: [u32; 5] = [0x0040C040, 0x00E09030, 0x00F060C0, 0x0040D0F0, 0x00F0F040];

/// Virtual-desktop pixels to diagram pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Xform {
    pub left: i32,
    pub top: i32,
    pub scale: f64,
    pub ox: i32,
    pub oy: i32,
}
impl Xform {
    pub fn point(&self, x: f64, y: f64) -> POINT {
        POINT {
            x: self.ox + ((x - self.left as f64) * self.scale).round() as i32,
            y: self.oy + ((y - self.top as f64) * self.scale).round() as i32,
        }
    }
    pub fn rect(&self, r: RECT) -> RECT {
        let a = self.point(r.left as f64, r.top as f64);
        let b = self.point(r.right as f64, r.bottom as f64);
        RECT {
            left: a.x,
            top: a.y,
            right: b.x,
            bottom: b.y,
        }
    }
}

pub struct Layout {
    pub screen: Xform,
    /// Monitor screens (inside bezels), in diagram pixels, same order as input.
    pub screens: Vec<RECT>,
    /// (neck, base) for monitors standing on the bottom row.
    pub stands: Vec<(RECT, RECT)>,
    pub bezel: i32,
    /// Touchpad body and its touch surface (physical aspect).
    pub pad: RECT,
    pub surface: RECT,
}

fn inset(r: RECT, d: i32) -> RECT {
    RECT {
        left: r.left + d,
        top: r.top + d,
        right: r.right - d,
        bottom: r.bottom - d,
    }
}

/// Arrange monitors above a touchpad inside `area`.
pub fn layout(
    area: RECT,
    virt: RECT,
    monitors: &[RECT],
    pad_aspect: f64,
    dpi: u32,
) -> Option<Layout> {
    let px = |n: i32| (n as i64 * dpi.max(96) as i64 / 96) as i32;
    let (w, h) = (area.right - area.left, area.bottom - area.top);
    let (vw, vh) = (virt.right - virt.left, virt.bottom - virt.top);
    if w < px(80) || h < px(90) || vw <= 0 || vh <= 0 || monitors.is_empty() {
        return None;
    }
    let bezel = px(4).max(2);
    let screens_box = RECT {
        left: area.left + bezel,
        top: area.top + bezel,
        right: area.right - bezel,
        bottom: area.top + h * 58 / 100 - bezel,
    };
    let (bw, bh) = (
        screens_box.right - screens_box.left,
        screens_box.bottom - screens_box.top,
    );
    if bw < 4 || bh < 4 {
        return None;
    }
    let scale = (bw as f64 / vw as f64).min(bh as f64 / vh as f64);
    let screen = Xform {
        left: virt.left,
        top: virt.top,
        scale,
        ox: screens_box.left + ((bw as f64 - vw as f64 * scale) / 2.0).round() as i32,
        oy: screens_box.bottom - (vh as f64 * scale).round() as i32,
    };
    let screens: Vec<RECT> = monitors.iter().map(|&m| screen.rect(m)).collect();
    let stand_bottom = area.top + h * 69 / 100;
    let stands = monitors
        .iter()
        .zip(&screens)
        .filter(|(m, _)| m.bottom == virt.bottom)
        .map(|(_, s)| {
            let sw = s.right - s.left;
            let cx = (s.left + s.right) / 2;
            let neck_w = (sw * 8 / 100).max(px(4));
            let base_w = (sw * 34 / 100).max(px(12));
            let base_h = px(3).max(2);
            let neck = RECT {
                left: cx - neck_w / 2,
                top: s.bottom + bezel,
                right: cx + neck_w / 2,
                bottom: stand_bottom - base_h,
            };
            let base = RECT {
                left: cx - base_w / 2,
                top: stand_bottom - base_h,
                right: cx + base_w / 2,
                bottom: stand_bottom,
            };
            (neck, base)
        })
        .collect();
    let pad_w = w * 50 / 100;
    let pad_box = RECT {
        left: area.left + (w - pad_w) / 2,
        top: area.top + h * 75 / 100,
        right: area.left + (w - pad_w) / 2 + pad_w,
        bottom: area.bottom,
    };
    let pad = fit_aspect(pad_box, pad_aspect).ok()?;
    let edge = ((pad.right - pad.left).min(pad.bottom - pad.top) * 8 / 100).max(2);
    let surface = fit_aspect(inset(pad, edge), pad_aspect).ok()?;
    Some(Layout {
        screen,
        screens,
        stands,
        bezel,
        pad,
        surface,
    })
}

/// The used part of the drawn touchpad surface.
pub fn used_surface(l: &Layout, input: NormRect) -> RECT {
    let s = l.surface;
    let (w, h) = ((s.right - s.left) as f64, (s.bottom - s.top) as f64);
    RECT {
        left: s.left + (input.left * w).round() as i32,
        top: s.top + (input.top * h).round() as i32,
        right: s.left + (input.right * w).round() as i32,
        bottom: s.top + (input.bottom * h).round() as i32,
    }
}

/// A normalized touchpad position drawn on the surface, and its mapped screen
/// position (None when it lies outside the used part of the touchpad).
pub fn contact_points(
    l: &Layout,
    region: RECT,
    input: NormRect,
    fx: f64,
    fy: f64,
) -> (POINT, Option<POINT>) {
    let fx = fx.clamp(0.0, 1.0);
    let fy = fy.clamp(0.0, 1.0);
    let s = l.surface;
    let on_pad = POINT {
        x: s.left + (fx * (s.right - s.left - 1) as f64).round() as i32,
        y: s.top + (fy * (s.bottom - s.top - 1) as f64).round() as i32,
    };
    let u = (fx - input.left) / (input.right - input.left);
    let v = (fy - input.top) / (input.bottom - input.top);
    let inside = (-1e-9..=1.0 + 1e-9).contains(&u) && (-1e-9..=1.0 + 1e-9).contains(&v);
    let on_screen = inside.then(|| {
        l.screen.point(
            region.left as f64 + u.clamp(0.0, 1.0) * (region.right - region.left - 1) as f64,
            region.top as f64 + v.clamp(0.0, 1.0) * (region.bottom - region.top - 1) as f64,
        )
    });
    (on_pad, on_screen)
}

/// Downscaled copy of the virtual desktop for the monitor illustrations.
pub struct Thumbnail {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    pub width: i32,
    pub virt: RECT,
}
impl Thumbnail {
    pub unsafe fn capture(virt: RECT, width: i32, height: i32) -> Result<Self, String> {
        if width < 1 || height < 1 {
            return Err("empty thumbnail".into());
        }
        let screen = GetDC(null_mut());
        if screen.is_null() {
            return Err(winerr("GetDC(screen)"));
        }
        let dc = CreateCompatibleDC(screen);
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        if dc.is_null() || bitmap.is_null() {
            if !dc.is_null() {
                DeleteDC(dc);
            }
            if !bitmap.is_null() {
                DeleteObject(bitmap);
            }
            ReleaseDC(null_mut(), screen);
            return Err(winerr("create thumbnail bitmap"));
        }
        let old = SelectObject(dc, bitmap);
        SetStretchBltMode(dc, HALFTONE);
        SetBrushOrgEx(dc, 0, 0, null_mut());
        // The screen DC origin is the primary monitor; secondary monitors
        // can have negative coordinates.
        let ok = StretchBlt(
            dc,
            0,
            0,
            width,
            height,
            screen,
            virt.left,
            virt.top,
            virt.right - virt.left,
            virt.bottom - virt.top,
            SRCCOPY,
        );
        ReleaseDC(null_mut(), screen);
        let thumb = Self {
            dc,
            bitmap,
            old,
            width,
            virt,
        };
        if ok == 0 {
            return Err(winerr("StretchBlt(desktop thumbnail)"));
        }
        Ok(thumb)
    }
    /// Source rectangle of a desktop area inside this thumbnail, or None if
    /// the thumbnail was taken for a different desktop arrangement.
    pub fn source(&self, virt: RECT, area: RECT) -> Option<RECT> {
        if crate::mapping::tuple(self.virt) != crate::mapping::tuple(virt) {
            return None;
        }
        Some(
            Xform {
                left: virt.left,
                top: virt.top,
                scale: self.width as f64 / (virt.right - virt.left).max(1) as f64,
                ox: 0,
                oy: 0,
            }
            .rect(area),
        )
    }
}
impl Drop for Thumbnail {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

pub struct Scene<'a> {
    pub virt: RECT,
    pub monitors: &'a [RECT],
    pub pad_aspect: Option<f64>,
    /// Mapped screen region in physical desktop pixels.
    pub region: Option<RECT>,
    /// The selected area before shape fitting (drawn when it differs).
    pub selected: Option<RECT>,
    /// Used part of the touchpad.
    pub pad_input: NormRect,
    /// Live normalized touchpad contacts.
    pub contacts: &'a [(f64, f64)],
    pub message: &'a str,
    pub dpi: u32,
}

unsafe fn fill(dc: HDC, r: &RECT, color: u32) {
    let brush = CreateSolidBrush(color);
    FillRect(dc, r, brush);
    DeleteObject(brush);
}

unsafe fn frame(dc: HDC, r: RECT, color: u32, width: i32) {
    for i in 0..width.max(1) {
        let brush = CreateSolidBrush(color);
        FrameRect(dc, &inset(r, i), brush);
        DeleteObject(brush);
    }
}

/// Paint the diagram into `area` and return the layout used, if any.
pub unsafe fn paint(
    dc: HDC,
    area: RECT,
    scene: &Scene,
    thumbnail: &mut Option<Thumbnail>,
    capture: bool,
) -> Option<Layout> {
    let px = |n: i32| (n as i64 * scene.dpi.max(96) as i64 / 96) as i32;
    let aspect = scene.pad_aspect.unwrap_or(1.6);
    let l = layout(area, scene.virt, scene.monitors, aspect, scene.dpi)?;
    let saved = SaveDC(dc);
    // Monitors with a desktop thumbnail, captured only on request.
    if capture {
        let shot = l.screen.rect(scene.virt);
        *thumbnail =
            Thumbnail::capture(scene.virt, shot.right - shot.left, shot.bottom - shot.top).ok();
    }
    for (neck, base) in &l.stands {
        fill(dc, neck, STAND);
        fill(dc, base, STAND);
    }
    for (s, m) in l.screens.iter().zip(scene.monitors) {
        fill(dc, &inset(*s, -l.bezel), BEZEL);
        match thumbnail
            .as_ref()
            .and_then(|t| Some((t, t.source(scene.virt, *m)?)))
        {
            Some((t, src)) => {
                SetStretchBltMode(dc, HALFTONE);
                StretchBlt(
                    dc,
                    s.left,
                    s.top,
                    s.right - s.left,
                    s.bottom - s.top,
                    t.dc,
                    src.left,
                    src.top,
                    src.right - src.left,
                    src.bottom - src.top,
                    SRCCOPY,
                );
            }
            None => fill(dc, s, EMPTY_SCREEN),
        }
    }
    // Touchpad body and touch surface.
    let body = CreateSolidBrush(PAD_BODY);
    let no_pen = GetStockObject(NULL_PEN);
    let old_pen = SelectObject(dc, no_pen);
    let old_brush = SelectObject(dc, body);
    let round = px(10);
    RoundRect(
        dc,
        l.pad.left,
        l.pad.top,
        l.pad.right,
        l.pad.bottom,
        round,
        round,
    );
    SelectObject(dc, old_brush);
    DeleteObject(body);
    let used = used_surface(&l, scene.pad_input);
    if scene.pad_input == NormRect::FULL {
        fill(dc, &l.surface, PAD_SURFACE);
    } else {
        fill(dc, &l.surface, PAD_UNUSED);
        fill(dc, &used, PAD_SURFACE);
    }
    if let Some(selected) = scene.selected {
        // Parts of the selected area the touchpad cannot reach (keep shape).
        if scene
            .region
            .is_none_or(|r| crate::mapping::tuple(r) != crate::mapping::tuple(selected))
        {
            frame(dc, l.screen.rect(selected), SELECTED, 1);
        }
    }
    if let Some(region) = scene.region {
        let r = l.screen.rect(region);
        let pen = CreatePen(PS_SOLID, px(1).max(1), RED);
        SelectObject(dc, pen);
        let s = used;
        for (a, b) in [
            ((s.left, s.top), (r.left, r.top)),
            ((s.right - 1, s.top), (r.right - 1, r.top)),
            ((s.left, s.bottom - 1), (r.left, r.bottom - 1)),
            ((s.right - 1, s.bottom - 1), (r.right - 1, r.bottom - 1)),
        ] {
            MoveToEx(dc, a.0, a.1, null_mut());
            LineTo(dc, b.0, b.1);
        }
        SelectObject(dc, no_pen);
        DeleteObject(pen);
        frame(dc, r, RED, px(2));
        frame(dc, used, RED, px(2));
        for (i, &(fx, fy)) in scene.contacts.iter().enumerate() {
            let (on_pad, on_screen) = contact_points(&l, region, scene.pad_input, fx, fy);
            let brush = CreateSolidBrush(CONTACT_COLORS[i % CONTACT_COLORS.len()]);
            SelectObject(dc, brush);
            for p in std::iter::once(on_pad).chain(on_screen) {
                let d = px(5);
                Ellipse(dc, p.x - d, p.y - d, p.x + d, p.y + d);
            }
            SelectObject(dc, old_brush);
            DeleteObject(brush);
        }
    }
    SelectObject(dc, old_pen);
    SelectObject(dc, old_brush);
    // Captions in the dialog font face (the DC default is the bold System font).
    let face = wide("Segoe UI");
    let font = CreateFontW(
        -px(12),
        0,
        0,
        0,
        FW_NORMAL as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        0,
        0,
        CLEARTYPE_QUALITY as u32,
        0,
        face.as_ptr(),
    );
    let old_font = if font.is_null() {
        null_mut()
    } else {
        SelectObject(dc, font)
    };
    SetBkMode(dc, TRANSPARENT as i32);
    SetTextColor(dc, GetSysColor(COLOR_GRAYTEXT));
    let pad_caption = if scene.pad_aspect.is_some() {
        "Touchpad (physical shape)"
    } else {
        "Touchpad (shape unavailable)"
    };
    let mut caption = RECT {
        left: area.left,
        top: l.pad.bottom - px(14),
        right: l.pad.left - px(8),
        bottom: l.pad.bottom,
    };
    let text = wide(pad_caption);
    DrawTextW(
        dc,
        text.as_ptr(),
        -1,
        &mut caption,
        DT_RIGHT | DT_BOTTOM | DT_SINGLELINE | DT_NOPREFIX | DT_NOCLIP,
    );
    if scene.region.is_none() && !scene.message.is_empty() {
        let screens = l.screen.rect(scene.virt);
        let mut box_rect = RECT {
            left: area.left,
            top: (screens.top + screens.bottom) / 2 - px(14),
            right: area.right,
            bottom: (screens.top + screens.bottom) / 2 + px(14),
        };
        fill(dc, &box_rect, GetSysColor(COLOR_BTNFACE));
        SetTextColor(dc, GetSysColor(COLOR_WINDOWTEXT));
        let text = wide(scene.message);
        DrawTextW(
            dc,
            text.as_ptr(),
            -1,
            &mut box_rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
        );
    }
    if !font.is_null() {
        SelectObject(dc, old_font);
        DeleteObject(font);
    }
    RestoreDC(dc, saved);
    Some(l)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::tuple;
    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }
    fn inside(inner: RECT, outer: RECT) -> bool {
        inner.left >= outer.left
            && inner.top >= outer.top
            && inner.right <= outer.right
            && inner.bottom <= outer.bottom
    }
    #[test]
    fn monitors_keep_their_arrangement_and_proportions() {
        // Secondary monitor to the left at negative coordinates, offset up.
        let monitors = [rect(0, 0, 2560, 1440), rect(-1920, -200, 0, 880)];
        let virt = rect(-1920, -200, 2560, 1440);
        let area = rect(16, 190, 516, 600);
        let l = layout(area, virt, &monitors, 1.6, 96).unwrap();
        let (primary, left) = (l.screens[0], l.screens[1]);
        assert!(inside(l.screen.rect(virt), area));
        assert_eq!(left.right, primary.left, "monitors stay adjacent");
        assert!(left.top < primary.top, "secondary sits higher");
        let ratio = |r: RECT| (r.right - r.left) as f64 / (r.bottom - r.top) as f64;
        assert!((ratio(primary) - 16.0 / 9.0).abs() < 0.05);
        assert!((ratio(left) - 1920.0 / 1080.0).abs() < 0.05);
        // Only the monitor on the bottom row gets a stand.
        assert_eq!(l.stands.len(), 1);
        assert!(l.stands[0].0.top >= primary.bottom);
        // The touchpad sits below the stands at its physical aspect.
        assert!(l.pad.top >= l.stands[0].1.bottom);
        assert!(inside(l.pad, area) && inside(l.surface, l.pad));
        assert!((ratio(l.surface) - 1.6).abs() < 0.05);
    }
    #[test]
    fn region_and_contacts_map_consistently() {
        let monitors = [rect(0, 0, 1920, 1080)];
        let virt = monitors[0];
        let l = layout(rect(0, 0, 400, 400), virt, &monitors, 1.5, 96).unwrap();
        let region = rect(960, 540, 1920, 1080);
        let r = l.screen.rect(region);
        let s = l.screens[0];
        assert_eq!((r.right, r.bottom), (s.right, s.bottom));
        assert_eq!(
            (r.left - s.left) * 2,
            s.right - s.left,
            "quarter region starts mid-screen"
        );
        let full = NormRect::FULL;
        let (pad, screen) = contact_points(&l, region, full, 0.0, 0.0);
        let screen = screen.unwrap();
        assert_eq!((pad.x, pad.y), (l.surface.left, l.surface.top));
        assert_eq!((screen.x, screen.y), (r.left, r.top));
        let (pad, screen) = contact_points(&l, region, full, 1.0, 1.0);
        let screen = screen.unwrap();
        assert_eq!((pad.x, pad.y), (l.surface.right - 1, l.surface.bottom - 1));
        assert!((screen.x - (r.right - 1)).abs() <= 1 && (screen.y - (r.bottom - 1)).abs() <= 1);
        // Out-of-range contacts are clamped onto the surface.
        let (pad, _) = contact_points(&l, region, full, 2.0, -1.0);
        assert_eq!((pad.x, pad.y), (l.surface.right - 1, l.surface.top));
        // With part of the pad used, its edges map to the region's edges and
        // contacts outside it have no screen position.
        let half = NormRect {
            left: 0.25,
            top: 0.0,
            right: 0.75,
            bottom: 1.0,
        };
        let used = used_surface(&l, half);
        let s = l.surface;
        assert_eq!(
            used.left - s.left,
            ((s.right - s.left) as f64 * 0.25).round() as i32
        );
        assert_eq!((used.top, used.bottom), (s.top, s.bottom));
        let (_, screen) = contact_points(&l, region, half, 0.25, 0.0);
        assert_eq!(screen.map(|p| (p.x, p.y)), Some((r.left, r.top)));
        let (_, screen) = contact_points(&l, region, half, 0.5, 0.5);
        let p = screen.unwrap();
        assert!((p.x - (r.left + r.right) / 2).abs() <= 1);
        assert!(contact_points(&l, region, half, 0.1, 0.5).1.is_none());
        assert!(contact_points(&l, region, half, 0.9, 0.5).1.is_none());
    }
    #[test]
    fn layout_scales_with_dpi_and_rejects_tiny_areas() {
        let monitors = [rect(0, 0, 1920, 1080)];
        assert!(layout(rect(0, 0, 70, 400), monitors[0], &monitors, 1.6, 96).is_none());
        assert!(layout(rect(0, 0, 400, 400), monitors[0], &[], 1.6, 96).is_none());
        let a = layout(rect(0, 0, 800, 700), monitors[0], &monitors, 1.6, 96).unwrap();
        let b = layout(rect(0, 0, 800, 700), monitors[0], &monitors, 1.6, 192).unwrap();
        assert_eq!(b.bezel, 2 * a.bezel);
        assert_eq!(tuple(a.pad), tuple(b.pad), "geometry depends on area only");
    }
}
