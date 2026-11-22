//! Phase 3 target selection and resolution.
//!
//! Pure geometry (normalized client areas, drag rectangles, virtual-screen
//! validation, hit policies and the dynamic target tracker) is separated from
//! Win32 queries so it can be tested without windows or touch injection.
//! All resolved bounds are physical desktop pixels with exclusive right/bottom.
use std::mem::size_of;
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{
        Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED},
        Gdi::{
            ClientToScreen, EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, HDC, HMONITOR,
            MONITORINFOEXW, MONITOR_DEFAULTTONULL,
        },
    },
    Security::*,
    System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken,
        PROCESS_QUERY_LIMITED_INFORMATION,
    },
    UI::{
        Controls::{GetComboBoxInfo, COMBOBOXINFO},
        HiDpi::{
            AreDpiAwarenessContextsEqual, GetDpiForWindow, GetThreadDpiAwarenessContext,
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        },
        WindowsAndMessaging::*,
    },
};

/// Smallest selectable target edge in physical pixels.
pub const MIN_TARGET_PX: i32 = 16;

/// A rectangle relative to a window's client area, 0.0..=1.0 on both axes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NormRect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}
impl NormRect {
    pub const FULL: Self = Self {
        left: 0.0,
        top: 0.0,
        right: 1.0,
        bottom: 1.0,
    };
    pub fn validate(self) -> Result<Self, String> {
        let v = [self.left, self.top, self.right, self.bottom];
        if v.iter().any(|n| !n.is_finite() || !(0.0..=1.0).contains(n))
            || self.right <= self.left
            || self.bottom <= self.top
        {
            return Err("invalid normalized window rectangle".into());
        }
        Ok(self)
    }
    /// Normalize a selected screen rectangle against the client area it was
    /// selected in. The selection is clipped to the client area first.
    pub fn from_screen(client: RECT, selected: RECT) -> Result<Self, String> {
        let w = client.right as f64 - client.left as f64;
        let h = client.bottom as f64 - client.top as f64;
        if w < 2.0 || h < 2.0 {
            return Err("target client area is empty".into());
        }
        let clipped = intersect(client, selected)
            .ok_or("selected area lies outside the target window's client area")?;
        Self {
            left: (clipped.left - client.left) as f64 / w,
            top: (clipped.top - client.top) as f64 / h,
            right: (clipped.right - client.left) as f64 / w,
            bottom: (clipped.bottom - client.top) as f64 / h,
        }
        .validate()
    }
    /// Scale into a client area in physical pixels; rounding is at most one pixel.
    pub fn apply(self, client: RECT) -> RECT {
        let w = client.right as f64 - client.left as f64;
        let h = client.bottom as f64 - client.top as f64;
        RECT {
            left: client.left + (self.left * w).round() as i32,
            top: client.top + (self.top * h).round() as i32,
            right: client.left + (self.right * w).round() as i32,
            bottom: client.top + (self.bottom * h).round() as i32,
        }
    }
}

pub fn intersect(a: RECT, b: RECT) -> Option<RECT> {
    let r = RECT {
        left: a.left.max(b.left),
        top: a.top.max(b.top),
        right: a.right.min(b.right),
        bottom: a.bottom.min(b.bottom),
    };
    (r.right > r.left && r.bottom > r.top).then_some(r)
}

pub fn size(r: RECT) -> (i64, i64) {
    (
        r.right as i64 - r.left as i64,
        r.bottom as i64 - r.top as i64,
    )
}

/// Rectangle covered by a drag between two inclusive pixel positions.
pub fn rect_from_drag(a: POINT, b: POINT) -> RECT {
    RECT {
        left: a.x.min(b.x),
        top: a.y.min(b.y),
        right: a.x.max(b.x).saturating_add(1),
        bottom: a.y.max(b.y).saturating_add(1),
    }
}

/// A screen rectangle must be large enough and inside the virtual desktop,
/// which can start at negative coordinates on secondary monitors.
pub fn validate_screen_rect(r: RECT, virtual_screen: RECT) -> Result<RECT, String> {
    let (w, h) = size(r);
    if w < MIN_TARGET_PX as i64 || h < MIN_TARGET_PX as i64 {
        return Err(format!(
            "Screen rectangle must be at least {MIN_TARGET_PX}×{MIN_TARGET_PX} physical pixels."
        ));
    }
    if r.left < virtual_screen.left
        || r.top < virtual_screen.top
        || r.right > virtual_screen.right
        || r.bottom > virtual_screen.bottom
    {
        return Err("Screen rectangle extends outside the current virtual desktop.".into());
    }
    Ok(r)
}

/// Validate a screen target against the current physical desktop at selection,
/// restore, and capture time.
pub fn validate_screen_target(bounds: RECT) -> Result<RECT, String> {
    require_physical_coordinates()?;
    let bounds = validate_screen_rect(bounds, unsafe { virtual_screen() })?;
    validate_screen_coverage(bounds, |p| {
        !unsafe { MonitorFromPoint(p, MONITOR_DEFAULTTONULL) }.is_null()
    })?;
    Ok(bounds)
}

fn validate_screen_coverage(
    bounds: RECT,
    on_monitor: impl Fn(POINT) -> bool,
) -> Result<(), String> {
    for p in sample_points(bounds, 3) {
        if !on_monitor(p) {
            return Err(format!(
                "Screen rectangle point ({}, {}) is not on any display. Select the area again.",
                p.x, p.y
            ));
        }
    }
    Ok(())
}

/// Points used for occlusion sampling and monitor coverage checks.
pub fn sample_points(r: RECT, per_axis: i32) -> Vec<POINT> {
    let n = per_axis.max(2);
    let (w, h) = size(r);
    if w < 1 || h < 1 {
        return vec![];
    }
    let mut points = Vec::with_capacity((n * n) as usize);
    for j in 0..n {
        for i in 0..n {
            points.push(POINT {
                x: (r.left as i64 + (w - 1) * i as i64 / (n - 1) as i64) as i32,
                y: (r.top as i64 + (h - 1) * j as i64 / (n - 1) as i64) as i32,
            });
        }
    }
    points
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

/// How the touchpad's shape is reconciled with the screen area's shape
/// (the "Touchpad area" setting).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum PadFit {
    /// Whole touchpad, undistorted: the screen area is letterboxed.
    #[default]
    KeepShape,
    /// Whole touchpad stretched over the whole screen area (distorts).
    Stretch,
    /// Whole screen area, undistorted: only a centered part of the touchpad
    /// with the area's shape is used; touches outside it are ignored.
    FillArea,
    /// A selected portion of the pad maps to the whole target.
    Portion(Option<NormRect>),
}
impl PadFit {
    pub fn label(self) -> &'static str {
        match self {
            Self::KeepShape => "keep-shape",
            Self::Stretch => "stretch",
            Self::FillArea => "fill-area",
            Self::Portion(_) => "portion",
        }
    }
    /// Whether the touchpad's physical shape is needed to map undistorted.
    pub fn needs_shape(self) -> bool {
        matches!(self, Self::KeepShape | Self::FillArea)
    }
}

/// Centered part of a touchpad (physical aspect `pad`) whose physical shape
/// matches `area`.
pub fn pad_crop(pad: f64, area: RECT) -> Result<NormRect, String> {
    let (w, h) = size(area);
    if !pad.is_finite() || pad <= 0.0 || w < 2 || h < 2 {
        return Err("invalid touchpad aspect or screen area".into());
    }
    let r = w as f64 / h as f64;
    if r < pad {
        let f = r / pad;
        NormRect {
            left: (1.0 - f) / 2.0,
            top: 0.0,
            right: (1.0 + f) / 2.0,
            bottom: 1.0,
        }
    } else {
        let f = pad / r;
        NormRect {
            left: 0.0,
            top: (1.0 - f) / 2.0,
            right: 1.0,
            bottom: (1.0 + f) / 2.0,
        }
    }
    .validate()
}

/// Apply a Touchpad area mode to a screen area. Returns the mapped bounds and
/// the used (normalized) touchpad region. Without physical geometry no shape
/// can be preserved, so the area and the whole touchpad are used.
pub fn fit_pad(area: RECT, aspect: Option<f64>, fit: PadFit) -> Result<(RECT, NormRect), String> {
    let (w, h) = size(area);
    if w < 2 || h < 2 {
        return Err("screen area is empty".into());
    }
    match (fit, aspect) {
        (PadFit::KeepShape, Some(a)) => Ok((fit_aspect(area, a)?, NormRect::FULL)),
        (PadFit::FillArea, Some(a)) => Ok((area, pad_crop(a, area)?)),
        (PadFit::Portion(Some(input)), _) => Ok((area, input.validate()?)),
        (PadFit::Portion(None), _) => {
            Err("Click Select pad area… and drag on the touchpad diagram.".into())
        }
        _ => Ok((area, NormRect::FULL)),
    }
}

/// Which window may receive a non-cleanup injected DOWN/UPDATE at a point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitPolicy {
    /// Only this exact window (the embedded diagnostic receiver).
    Exact(usize),
    /// This top-level window, its children, and popups associated with it.
    SameRoot(usize),
    /// Any window: a screen rectangle has no window ownership.
    Any,
}
impl HitPolicy {
    pub fn accepts(self, hit: usize, hit_root: usize) -> bool {
        match self {
            Self::Exact(w) => hit != 0 && hit == w,
            Self::SameRoot(w) => hit != 0 && hit_root == w,
            Self::Any => true,
        }
    }

    /// Hit-test a desktop point. Separate popup windows can qualify through
    /// ownership or their association with the target's active Win32 control.
    /// Returns the hit window and its root when the point is rejected.
    pub fn hit_test(self, point: POINT) -> Result<(), (usize, usize)> {
        unsafe {
            let hit = WindowFromPoint(point);
            let root = if hit.is_null() {
                std::ptr::null_mut()
            } else {
                GetAncestor(hit, GA_ROOT)
            };
            if self.accepts(hit as usize, root as usize) {
                return Ok(());
            }
            if let Self::SameRoot(target) = self {
                if !root.is_null() && belongs_to_target(target as HWND, root) {
                    return Ok(());
                }
            }
            Err((hit as usize, root as usize))
        }
    }
}

unsafe fn belongs_to_target(target: HWND, root: HWND) -> bool {
    if GetAncestor(root, GA_ROOTOWNER) == target {
        return true;
    }

    let mut class = [0u16; 16];
    let len = GetClassNameW(root, class.as_mut_ptr(), class.len() as i32) as usize;
    let is_menu = class[..len].iter().copied().eq("#32768".encode_utf16());
    let is_combo_list = class[..len].iter().copied().eq("ComboLBox".encode_utf16());
    if !is_menu && !is_combo_list {
        return false;
    }

    let thread = GetWindowThreadProcessId(root, std::ptr::null_mut());
    if thread == 0 || thread != GetWindowThreadProcessId(target, std::ptr::null_mut()) {
        return false;
    }
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    if GetGUIThreadInfo(thread, &mut info) == 0 {
        return false;
    }
    if is_menu {
        return info.flags & (GUI_INMENUMODE | GUI_POPUPMENUMODE | GUI_SYSTEMMENUMODE) != 0
            && GetAncestor(info.hwndMenuOwner, GA_ROOTOWNER) == target;
    }

    // A combo's focus may be on its edit control. Match the actual list
    // handle returned for that combo, not another list on the same thread.
    let focus = info.hwndFocus;
    if GetAncestor(focus, GA_ROOTOWNER) != target {
        return false;
    }
    [focus, GetAncestor(focus, GA_PARENT)]
        .into_iter()
        .any(|wnd| {
            let mut combo = COMBOBOXINFO {
                cbSize: size_of::<COMBOBOXINFO>() as u32,
                ..Default::default()
            };
            !wnd.is_null() && GetComboBoxInfo(wnd, &mut combo) != 0 && combo.hwndList == root
        })
}

pub const TEST_AREA_INSET: i32 = 16;
pub const DIAGNOSTICS_HEADER_HEIGHT: i32 = 190;

#[derive(Clone, Copy)]
pub enum MappingTarget {
    TestArea { diagnostics: bool },
    ScreenRectangle { bounds: RECT },
    Window { handle: usize },
    WindowRectangle { handle: usize, area: NormRect },
}

#[derive(Clone, Copy)]
pub struct ResolvedTarget {
    pub spec: MappingTarget,
    /// Diagnostic receiver or target top-level window; 0 for screen rectangles.
    pub window: usize,
    pub bounds: RECT,
    pub hit: HitPolicy,
    /// Physical touchpad aspect used when the target is re-resolved.
    pub aspect: Option<f64>,
    pub fit: PadFit,
    /// Used part of the touchpad, normalized to its logical range.
    pub input: NormRect,
}

/// Current availability of a dynamic target.
#[derive(Clone)]
pub enum Observation {
    Ready(RECT),
    Paused(String),
    Gone(String),
}

#[derive(Clone)]
pub enum Decision {
    Keep,
    Retarget(RECT),
    Pause(String),
    Resume(RECT),
    Stop(String),
}

/// RECT from windows-sys has no Debug/PartialEq; compare and print as tuples.
pub fn tuple(r: RECT) -> (i32, i32, i32, i32) {
    (r.left, r.top, r.right, r.bottom)
}
impl PartialEq for MappingTarget {
    fn eq(&self, other: &Self) -> bool {
        use MappingTarget::*;
        match (*self, *other) {
            (TestArea { diagnostics: a }, TestArea { diagnostics: b }) => a == b,
            (ScreenRectangle { bounds: a }, ScreenRectangle { bounds: b }) => tuple(a) == tuple(b),
            (Window { handle: a }, Window { handle: b }) => a == b,
            (WindowRectangle { handle: a, area: x }, WindowRectangle { handle: b, area: y }) => {
                a == b && x == y
            }
            _ => false,
        }
    }
}
impl std::fmt::Debug for MappingTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::TestArea { diagnostics } => f
                .debug_struct("TestArea")
                .field("diagnostics", &diagnostics)
                .finish(),
            Self::ScreenRectangle { bounds } => write!(f, "ScreenRectangle{:?}", tuple(bounds)),
            Self::Window { handle } => write!(f, "Window(0x{handle:X})"),
            Self::WindowRectangle { handle, area } => {
                write!(f, "WindowRectangle(0x{handle:X}, {area:?})")
            }
        }
    }
}
impl PartialEq for Observation {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Ready(a), Self::Ready(b)) => tuple(*a) == tuple(*b),
            (Self::Paused(a), Self::Paused(b)) | (Self::Gone(a), Self::Gone(b)) => a == b,
            _ => false,
        }
    }
}
impl std::fmt::Debug for Observation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready(r) => write!(f, "Ready{:?}", tuple(*r)),
            Self::Paused(r) => write!(f, "Paused({r})"),
            Self::Gone(r) => write!(f, "Gone({r})"),
        }
    }
}
impl PartialEq for Decision {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Keep, Self::Keep) => true,
            (Self::Retarget(a), Self::Retarget(b)) | (Self::Resume(a), Self::Resume(b)) => {
                tuple(*a) == tuple(*b)
            }
            (Self::Pause(a), Self::Pause(b)) | (Self::Stop(a), Self::Stop(b)) => a == b,
            _ => false,
        }
    }
}
impl std::fmt::Debug for Decision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keep => write!(f, "Keep"),
            Self::Retarget(r) => write!(f, "Retarget{:?}", tuple(*r)),
            Self::Resume(r) => write!(f, "Resume{:?}", tuple(*r)),
            Self::Pause(r) => write!(f, "Pause({r})"),
            Self::Stop(r) => write!(f, "Stop({r})"),
        }
    }
}

/// Follows a window target during a session. Movement/resizing retargets,
/// minimization/hiding pauses, destruction stops. Every transition except
/// Keep requires the caller to release active contacts first.
#[derive(Clone)]
pub struct Tracker {
    bounds: RECT,
    paused: bool,
}
impl Tracker {
    pub fn new(bounds: RECT) -> Self {
        Self {
            bounds,
            paused: false,
        }
    }
    #[cfg(test)]
    pub fn bounds(&self) -> RECT {
        self.bounds
    }
    pub fn paused(&self) -> bool {
        self.paused
    }
    pub fn observe(&mut self, observation: Observation) -> Decision {
        match observation {
            Observation::Gone(reason) => Decision::Stop(reason),
            Observation::Paused(_) if self.paused => Decision::Keep,
            Observation::Paused(reason) => {
                self.paused = true;
                Decision::Pause(reason)
            }
            Observation::Ready(bounds) if self.paused => {
                self.paused = false;
                self.bounds = bounds;
                Decision::Resume(bounds)
            }
            Observation::Ready(bounds) if tuple(bounds) != tuple(self.bounds) => {
                self.bounds = bounds;
                Decision::Retarget(bounds)
            }
            Observation::Ready(_) => Decision::Keep,
        }
    }
}

pub fn describe(r: RECT) -> String {
    let (w, h) = size(r);
    format!("{w}×{h} px at ({}, {})", r.left, r.top)
}

impl MappingTarget {
    pub fn label(&self) -> &'static str {
        match self {
            Self::TestArea { .. } => "diagnostic",
            Self::ScreenRectangle { .. } => "screen-rectangle",
            Self::Window { .. } => "window",
            Self::WindowRectangle { .. } => "window-rectangle",
        }
    }
    pub fn is_diagnostic(&self) -> bool {
        matches!(self, Self::TestArea { .. })
    }
    pub fn window_handle(&self) -> Option<usize> {
        match *self {
            Self::Window { handle } | Self::WindowRectangle { handle, .. } => Some(handle),
            _ => None,
        }
    }
    #[cfg(test)]
    pub fn resolve_with_aspect(self, window: HWND, aspect: f64) -> Result<ResolvedTarget, String> {
        self.resolve_fitted(window, Some(aspect), PadFit::KeepShape)
    }
    /// Resolve and apply a Touchpad area mode.
    pub fn resolve_fitted(
        self,
        window: HWND,
        aspect: Option<f64>,
        fit: PadFit,
    ) -> Result<ResolvedTarget, String> {
        let mut target = self.resolve(window)?;
        // The built-in test area can fit itself to the pad. External targets
        // have fixed bounds, so Force proportions crops the pad for those.
        let fit = match (self, fit) {
            (Self::TestArea { .. }, PadFit::FillArea) => PadFit::KeepShape,
            _ => fit,
        };
        let (bounds, input) = fit_pad(target.bounds, aspect, fit)?;
        target.bounds = bounds;
        target.input = input;
        target.aspect = aspect;
        target.fit = fit;
        Ok(target)
    }
    pub fn resolve(self, diagnostic_window: HWND) -> Result<ResolvedTarget, String> {
        match self {
            Self::TestArea { diagnostics } => unsafe {
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
                    spec: self,
                    window: diagnostic_window as usize,
                    bounds: RECT {
                        left: origin.x + px(TEST_AREA_INSET),
                        top: origin.y
                            + px(if diagnostics {
                                DIAGNOSTICS_HEADER_HEIGHT
                            } else {
                                TEST_AREA_INSET
                            }),
                        right: origin.x + client.right - px(TEST_AREA_INSET),
                        bottom: origin.y + client.bottom - px(20),
                    },
                    hit: HitPolicy::Exact(diagnostic_window as usize),
                    aspect: None,
                    fit: PadFit::Stretch,
                    input: NormRect::FULL,
                })
            },
            Self::ScreenRectangle { bounds } => {
                let bounds = validate_screen_target(bounds)?;
                Ok(ResolvedTarget {
                    spec: self,
                    window: 0,
                    bounds,
                    hit: HitPolicy::Any,
                    aspect: None,
                    fit: PadFit::Stretch,
                    input: NormRect::FULL,
                })
            }
            Self::Window { handle } | Self::WindowRectangle { handle, .. } => {
                require_physical_coordinates()?;
                check_foreign_window(handle)?;
                match self.observe_bounds() {
                    Observation::Ready(bounds) => Ok(ResolvedTarget {
                        spec: self,
                        window: handle,
                        bounds,
                        hit: HitPolicy::SameRoot(handle),
                        aspect: None,
                        fit: PadFit::Stretch,
                        input: NormRect::FULL,
                    }),
                    Observation::Paused(reason) | Observation::Gone(reason) => Err(format!(
                        "Target window unavailable: {reason}. Restore it before capturing."
                    )),
                }
            }
        }
    }
    /// Unfitted bounds of a window target in physical pixels.
    pub fn observe_bounds(&self) -> Observation {
        let (handle, area) = match *self {
            Self::Window { handle } => (handle, NormRect::FULL),
            Self::WindowRectangle { handle, area } => (handle, area),
            Self::ScreenRectangle { bounds } => return Observation::Ready(bounds),
            Self::TestArea { .. } => return Observation::Gone("not a dynamic target".into()),
        };
        let client = match unsafe { client_screen_rect(handle) } {
            Ok(client) => client,
            Err(o) => return o,
        };
        let r = area.apply(client);
        let (w, h) = size(r);
        if w < MIN_TARGET_PX as i64 || h < MIN_TARGET_PX as i64 {
            return Observation::Paused("target area is too small".into());
        }
        Observation::Ready(r)
    }
}

impl ResolvedTarget {
    /// Re-resolve a window target, applying the session's physical aspect.
    pub fn observe(&self) -> Observation {
        match self.spec.observe_bounds() {
            Observation::Ready(r) => match fit_pad(r, self.aspect, self.fit) {
                Ok((fitted, _)) => Observation::Ready(fitted),
                Err(_) => Observation::Paused("target area cannot fit touchpad aspect".into()),
            },
            other => other,
        }
    }
    /// Used touchpad region for (already fitted) bounds; FillArea follows the
    /// window's current shape.
    pub fn input_for(&self, bounds: RECT) -> NormRect {
        fit_pad(bounds, self.aspect, self.fit)
            .map(|(_, input)| input)
            .unwrap_or(NormRect::FULL)
    }
    pub fn dynamic(&self) -> bool {
        self.spec.window_handle().is_some()
    }
}

/// Coordinate APIs return physical pixels only for per-monitor-aware threads.
fn require_physical_coordinates() -> Result<(), String> {
    if unsafe {
        AreDpiAwarenessContextsEqual(
            GetThreadDpiAwarenessContext(),
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        )
    } == 0
    {
        return Err("Per-Monitor DPI Awareness V2 is required to resolve physical pixels.".into());
    }
    Ok(())
}

pub unsafe fn virtual_screen() -> RECT {
    let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
    let top = GetSystemMetrics(SM_YVIRTUALSCREEN);
    RECT {
        left,
        top,
        right: left + GetSystemMetrics(SM_CXVIRTUALSCREEN),
        bottom: top + GetSystemMetrics(SM_CYVIRTUALSCREEN),
    }
}

pub unsafe fn cloaked(hwnd: HWND) -> bool {
    let mut value: u32 = 0;
    DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED as u32,
        (&mut value as *mut u32).cast(),
        size_of::<u32>() as u32,
    ) == 0
        && value != 0
}

/// Client area in physical screen coordinates. Both corners pass through
/// ClientToScreen, so DPI virtualization of other processes is consistent.
unsafe fn client_screen_rect(handle: usize) -> Result<RECT, Observation> {
    let hwnd = handle as HWND;
    if handle == 0 || IsWindow(hwnd) == 0 {
        return Err(Observation::Gone("target window closed".into()));
    }
    if IsIconic(hwnd) != 0 {
        return Err(Observation::Paused("target window minimized".into()));
    }
    if IsWindowVisible(hwnd) == 0 {
        return Err(Observation::Paused("target window hidden".into()));
    }
    if cloaked(hwnd) {
        return Err(Observation::Paused(
            "target window cloaked (another virtual desktop or suspended)".into(),
        ));
    }
    let mut client = RECT::default();
    if GetClientRect(hwnd, &mut client) == 0 {
        return Err(if IsWindow(hwnd) == 0 {
            Observation::Gone("target window closed".into())
        } else {
            Observation::Paused("target client area unavailable".into())
        });
    }
    let mut a = POINT { x: 0, y: 0 };
    let mut b = POINT {
        x: client.right,
        y: client.bottom,
    };
    if ClientToScreen(hwnd, &mut a) == 0 || ClientToScreen(hwnd, &mut b) == 0 {
        return Err(Observation::Paused("target position unavailable".into()));
    }
    let r = RECT {
        left: a.x,
        top: a.y,
        right: b.x,
        bottom: b.y,
    };
    let (w, h) = size(r);
    if w < 2 || h < 2 {
        return Err(Observation::Paused("target client area is empty".into()));
    }
    Ok(r)
}

/// Client area of a window target in physical screen coordinates.
pub fn window_client(handle: usize) -> Result<RECT, String> {
    require_physical_coordinates()?;
    unsafe { client_screen_rect(handle) }.map_err(|o| match o {
        Observation::Paused(r) | Observation::Gone(r) => r,
        Observation::Ready(_) => unreachable!(),
    })
}

fn check_foreign_window(handle: usize) -> Result<(), String> {
    let hwnd = handle as HWND;
    unsafe {
        if handle == 0 || IsWindow(hwnd) == 0 {
            return Err("Select an existing application window.".into());
        }
        if GetAncestor(hwnd, GA_ROOT) != hwnd {
            return Err("Select a top-level application window.".into());
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == GetCurrentProcessId() {
            return Err(
                "Pad2Touch's own window cannot be an application target; use Test area.".into(),
            );
        }
    }
    Ok(())
}

#[derive(Clone)]
pub struct MonitorEntry {
    /// Full monitor rectangle in physical desktop pixels.
    pub rect: RECT,
    pub primary: bool,
    /// Display device name, stable across refreshes (e.g. \\.\DISPLAY2).
    pub device: String,
}

/// Order monitors as users number them: primary first, then left to right,
/// top to bottom.
pub fn order_monitors(monitors: &mut [MonitorEntry]) {
    monitors.sort_by_key(|m| (!m.primary, m.rect.left, m.rect.top));
}

/// Connected display monitors in physical pixels.
pub fn monitors() -> Vec<MonitorEntry> {
    unsafe extern "system" fn each(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        l: LPARAM,
    ) -> windows_sys::core::BOOL {
        let out = &mut *(l as *mut Vec<MonitorEntry>);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(monitor, (&mut info as *mut MONITORINFOEXW).cast()) != 0 {
            let len = info
                .szDevice
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(info.szDevice.len());
            out.push(MonitorEntry {
                rect: info.monitorInfo.rcMonitor,
                primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                device: String::from_utf16_lossy(&info.szDevice[..len]),
            });
        }
        1
    }
    let mut out = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(each),
            &mut out as *mut _ as LPARAM,
        );
    }
    order_monitors(&mut out);
    out
}

#[derive(Clone, Debug)]
pub struct WindowEntry {
    pub handle: usize,
    pub title: String,
    pub minimized: bool,
}

/// Visible top-level windows of other processes, in Z order. Titles are shown
/// for selection only; logs record handles and geometry, never titles.
pub fn list_windows() -> Vec<WindowEntry> {
    unsafe extern "system" fn each(hwnd: HWND, l: LPARAM) -> windows_sys::core::BOOL {
        let out = &mut *(l as *mut Vec<WindowEntry>);
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if pid == GetCurrentProcessId()
            || IsWindowVisible(hwnd) == 0
            || cloaked(hwnd)
            || ex & WS_EX_TOOLWINDOW != 0
            || !GetWindow(hwnd, GW_OWNER).is_null()
        {
            return 1;
        }
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return 1;
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        if title.trim().is_empty() {
            return 1;
        }
        out.push(WindowEntry {
            handle: hwnd as usize,
            title,
            minimized: IsIconic(hwnd) != 0,
        });
        1
    }
    let mut out = Vec::new();
    unsafe {
        EnumWindows(Some(each), &mut out as *mut _ as LPARAM);
    }
    out
}

unsafe fn integrity_of(process: HANDLE) -> Option<u32> {
    let mut token: HANDLE = std::ptr::null_mut();
    if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
        return None;
    }
    let mut needed = 0;
    GetTokenInformation(
        token,
        TokenIntegrityLevel,
        std::ptr::null_mut(),
        0,
        &mut needed,
    );
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8).max(1)];
    let ok = GetTokenInformation(
        token,
        TokenIntegrityLevel,
        buffer.as_mut_ptr().cast(),
        (buffer.len() * 8) as u32,
        &mut needed,
    ) != 0;
    CloseHandle(token);
    if !ok {
        return None;
    }
    let label = &*(buffer.as_ptr() as *const TOKEN_MANDATORY_LABEL);
    let sid = label.Label.Sid;
    let count = *GetSidSubAuthorityCount(sid);
    if count == 0 {
        return None;
    }
    Some(*GetSidSubAuthority(sid, count as u32 - 1))
}

/// Warn when User Interface Privilege Isolation may block injected input.
pub fn integrity_warning(handle: usize) -> Option<String> {
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(handle as HWND, &mut pid);
        if pid == 0 {
            return None;
        }
        let own = integrity_of(GetCurrentProcess())?;
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return Some(
                "Target process integrity unavailable; Windows may block injected touch.".into(),
            );
        }
        let theirs = integrity_of(process);
        CloseHandle(process);
        match theirs {
            Some(level) if level > own => Some(
                "Target runs at higher integrity (e.g. as administrator); Windows UIPI may block injected touch."
                    .into(),
            ),
            None => Some(
                "Target process integrity unavailable; Windows may block injected touch.".into(),
            ),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }
    #[test]
    fn physical_shape_is_centered_inside_wide_tall_and_negative_screen_bounds() {
        for area in [
            rect(-1600, 30, -600, 330),
            rect(10, -800, 310, 100),
            rect(0, 0, 1001, 777),
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
            assert!(fit_aspect(rect(0, 0, 100, 100), ratio).is_err());
        }
    }
    #[test]
    fn invalid_targets_never_fall_back_to_diagnostic_injection() {
        for target in [
            MappingTarget::ScreenRectangle {
                bounds: RECT::default(),
            },
            MappingTarget::Window { handle: 0 },
            MappingTarget::WindowRectangle {
                handle: 0,
                area: NormRect::FULL,
            },
        ] {
            assert!(target.resolve(std::ptr::null_mut()).is_err());
        }
    }
    #[test]
    fn drag_rectangles_are_order_independent_and_inclusive() {
        let a = POINT { x: -1900, y: 40 };
        let b = POINT { x: -1500, y: -200 };
        assert_eq!(tuple(rect_from_drag(a, b)), (-1900, -200, -1499, 41));
        assert_eq!(tuple(rect_from_drag(b, a)), tuple(rect_from_drag(a, b)));
        assert_eq!(tuple(rect_from_drag(a, a)), (-1900, 40, -1899, 41));
    }
    #[test]
    fn screen_rectangles_accept_negative_virtual_desktop_and_reject_outside() {
        let desktop = rect(-2560, -360, 1920, 1080);
        assert!(validate_screen_rect(rect(-2560, -360, -1280, 360), desktop).is_ok());
        assert!(validate_screen_rect(rect(0, 0, 1920, 1080), desktop).is_ok());
        assert!(validate_screen_rect(rect(-2561, 0, -1000, 300), desktop).is_err());
        assert!(validate_screen_rect(rect(100, 100, 1921, 300), desktop).is_err());
        assert!(validate_screen_rect(rect(0, 0, 15, 300), desktop).is_err());
        assert!(validate_screen_rect(rect(0, 0, 300, 10), desktop).is_err());
        assert!(validate_screen_rect(rect(300, 0, 0, 300), desktop).is_err());
    }
    #[test]
    fn screen_target_rejects_a_gap_after_moving_a_shorter_monitor() {
        let area = rect(2000, 100, 2200, 300);
        let desktop = rect(0, 0, 3200, 1080);
        assert!(validate_screen_rect(area, desktop).is_ok());
        let covered =
            |top| validate_screen_coverage(area, |p| p.x < 1920 || (top..top + 720).contains(&p.y));
        assert!(covered(0).is_ok());
        assert!(covered(360).unwrap_err().contains("not on any display"));
    }
    #[test]
    fn normalized_window_area_follows_client_moves_and_dpi_scaling() {
        // Selected at 100% scaling on the primary monitor.
        let client = rect(100, 200, 1100, 800);
        let selected = rect(350, 350, 850, 650);
        let area = NormRect::from_screen(client, selected).unwrap();
        assert_eq!(tuple(area.apply(client)), tuple(selected));
        // The same window moved to a negative-coordinate monitor at 150% and 200%.
        for (scale, origin) in [(1.5, (-2400, -300)), (2.0, (-3000, 100)), (1.25, (0, 0))] {
            let w = (1000.0 * scale) as i32;
            let h = (600.0 * scale) as i32;
            let moved = rect(origin.0, origin.1, origin.0 + w, origin.1 + h);
            let r = area.apply(moved);
            let expect = |frac: f64, base: i32, len: i32| base + (frac * len as f64).round() as i32;
            assert_eq!(r.left, expect(0.25, moved.left, w));
            assert_eq!(r.top, expect(0.25, moved.top, h));
            assert_eq!(r.right, expect(0.75, moved.left, w));
            assert_eq!(r.bottom, expect(0.75, moved.top, h));
        }
        assert_eq!(tuple(NormRect::FULL.apply(client)), tuple(client));
    }
    #[test]
    fn normalized_selection_is_clipped_and_validated() {
        let client = rect(0, 0, 400, 200);
        let area = NormRect::from_screen(client, rect(-50, -50, 200, 100)).unwrap();
        assert_eq!(
            area,
            NormRect {
                left: 0.0,
                top: 0.0,
                right: 0.5,
                bottom: 0.5
            }
        );
        assert!(NormRect::from_screen(client, rect(500, 0, 600, 100)).is_err());
        assert!(NormRect::from_screen(rect(0, 0, 1, 1), client).is_err());
        for bad in [
            NormRect {
                left: 0.5,
                top: 0.0,
                right: 0.5,
                bottom: 1.0,
            },
            NormRect {
                left: -0.1,
                top: 0.0,
                right: 0.5,
                bottom: 1.0,
            },
            NormRect {
                left: 0.0,
                top: 0.0,
                right: f64::NAN,
                bottom: 1.0,
            },
        ] {
            assert!(bad.validate().is_err());
        }
    }
    #[test]
    fn hit_policies_allow_children_only_for_application_targets() {
        assert!(HitPolicy::Exact(5).accepts(5, 1));
        assert!(!HitPolicy::Exact(5).accepts(6, 5));
        assert!(!HitPolicy::Exact(5).accepts(0, 0));
        assert!(HitPolicy::SameRoot(9).accepts(9, 9));
        assert!(HitPolicy::SameRoot(9).accepts(42, 9));
        assert!(!HitPolicy::SameRoot(9).accepts(42, 7));
        assert!(!HitPolicy::SameRoot(9).accepts(0, 0));
        assert!(HitPolicy::Any.accepts(0, 0));
        assert!(HitPolicy::Any.accepts(3, 4));
    }

    #[test]
    fn popup_ownership_rejects_unrelated_windows_on_the_same_thread() {
        use std::ptr::{null, null_mut};

        unsafe {
            let class = crate::experiment::wide("STATIC");
            let create = |owner| {
                let hwnd = CreateWindowExW(
                    0,
                    class.as_ptr(),
                    null(),
                    WS_POPUP,
                    0,
                    0,
                    100,
                    100,
                    owner,
                    null_mut(),
                    null_mut(),
                    null(),
                );
                assert!(!hwnd.is_null());
                hwnd
            };
            let target = create(null_mut());
            let unrelated = create(null_mut());
            let popup = create(target);
            let nested = create(popup);
            let unrelated_popup = create(unrelated);

            let belongs = [popup, nested, unrelated, unrelated_popup]
                .map(|window| belongs_to_target(target, window));
            for window in [unrelated_popup, nested, popup, unrelated, target] {
                DestroyWindow(window);
            }
            assert_eq!(belongs, [true, true, false, false]);
        }
    }
    #[test]
    fn sample_grid_covers_corners_and_center_of_negative_rectangles() {
        let r = rect(-1001, -501, -1, -1);
        let p = sample_points(r, 3);
        assert_eq!(p.len(), 9);
        assert_eq!((p[0].x, p[0].y), (-1001, -501));
        assert_eq!((p[4].x, p[4].y), (-502, -252));
        assert_eq!((p[8].x, p[8].y), (-2, -2));
        assert!(p
            .iter()
            .all(|q| q.x >= r.left && q.x < r.right && q.y >= r.top && q.y < r.bottom));
        assert!(sample_points(rect(0, 0, 0, 10), 3).is_empty());
    }
    #[test]
    fn touchpad_area_modes_reconcile_pad_and_screen_shapes() {
        let pad = 1.6;
        let monitor = rect(-1920, 0, 0, 1080); // 16:9, wider than the pad
        let phone = rect(100, 100, 460, 900); // 0.45, narrower than the pad
                                              // Keep shape: whole pad, area letterboxed to the pad's shape.
        let (b, input) = fit_pad(monitor, Some(pad), PadFit::KeepShape).unwrap();
        assert_eq!(input, NormRect::FULL);
        assert_eq!(tuple(b), (-1824, 0, -96, 1080));
        // Stretch: whole pad over the whole area, even without geometry.
        for aspect in [Some(pad), None] {
            let (b, input) = fit_pad(phone, aspect, PadFit::Stretch).unwrap();
            assert_eq!((tuple(b), input), (tuple(phone), NormRect::FULL));
        }
        // Fill area: whole area, centered pad part with the area's shape.
        let (b, input) = fit_pad(phone, Some(pad), PadFit::FillArea).unwrap();
        assert_eq!(tuple(b), tuple(phone));
        let used = (input.right - input.left) * pad / (input.bottom - input.top);
        assert!(
            (used - 360.0 / 800.0).abs() < 1e-9,
            "used pad shape = area shape"
        );
        assert!((input.left + input.right - 1.0).abs() < 1e-9, "centered");
        let (_, input) = fit_pad(monitor, Some(pad), PadFit::FillArea).unwrap();
        assert_eq!((input.left, input.right), (0.0, 1.0));
        assert!((input.bottom - input.top - pad / (16.0 / 9.0)).abs() < 1e-9);
        // Without physical geometry, shapes cannot be preserved.
        for fit in [PadFit::KeepShape, PadFit::FillArea] {
            let (b, input) = fit_pad(phone, None, fit).unwrap();
            assert_eq!((tuple(b), input), (tuple(phone), NormRect::FULL));
            assert!(fit.needs_shape());
        }
        assert!(!PadFit::Stretch.needs_shape());
        assert!(fit_pad(rect(0, 0, 1, 100), Some(pad), PadFit::Stretch).is_err());
        assert!(pad_crop(f64::NAN, phone).is_err());
    }
    #[test]
    fn monitors_are_numbered_primary_first_then_left_to_right() {
        let m = |left: i32, top: i32, primary: bool, device: &str| MonitorEntry {
            rect: rect(left, top, left + 100, top + 100),
            primary,
            device: device.into(),
        };
        let mut list = vec![
            m(100, 0, false, "C"),
            m(-100, 50, false, "B"),
            m(0, 0, true, "A"),
            m(-100, -50, false, "D"),
        ];
        order_monitors(&mut list);
        let names: Vec<_> = list.iter().map(|m| m.device.as_str()).collect();
        assert_eq!(names, ["A", "D", "B", "C"]);
    }
    #[test]
    fn tracker_retargets_pauses_resumes_and_stops() {
        let a = rect(0, 0, 160, 100);
        let b = rect(40, 30, 200, 130);
        let mut t = Tracker::new(a);
        assert_eq!(t.observe(Observation::Ready(a)), Decision::Keep);
        assert_eq!(t.observe(Observation::Ready(b)), Decision::Retarget(b));
        assert_eq!(tuple(t.bounds()), tuple(b));
        assert_eq!(
            t.observe(Observation::Paused("minimized".into())),
            Decision::Pause("minimized".into())
        );
        assert!(t.paused());
        // Repeated pause observations do not cause repeated releases.
        assert_eq!(
            t.observe(Observation::Paused("hidden".into())),
            Decision::Keep
        );
        // Restoration resumes even at unchanged bounds, requiring a clear frame.
        assert_eq!(t.observe(Observation::Ready(b)), Decision::Resume(b));
        assert!(!t.paused());
        assert_eq!(
            t.observe(Observation::Gone("closed".into())),
            Decision::Stop("closed".into())
        );
    }
}
