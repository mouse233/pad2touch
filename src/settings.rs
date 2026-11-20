//! Persisted GUI settings, stored next to the gesture recovery journals.
//!
//! Every field is optional: `None` means "the user never changed this", so the
//! control keeps its startup default. That keeps the schema forward-compatible
//! and lets the gesture check boxes keep falling back to the Windows build
//! family instead of guessing a stored value.
use crate::{control::TakeoverMode, gesture_backend, mapping::NormRect};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use windows_sys::Win32::Foundation::RECT;

/// Schema version written by this build. A file with another version is
/// ignored as a whole rather than half-applied.
pub const FORMAT: u32 = 1;

#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub format: u32,
    /// Raw Input device path; indexes are not stable across launches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inject: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub touch_feedback: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suppress_touchpad: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suppress_touchscreen: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_mode: Option<Mode>,
    /// Virtual-key code of the global chord, `VK_F8`..`VK_F11`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shortcut: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen_rect: Option<Rect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_area: Option<Area>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pad_area: Option<Area>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pad_fit: Option<Fit>,
}

/// Start mode by name, never by combo index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Manual,
    Hold,
    Toggle,
}
impl From<TakeoverMode> for Mode {
    fn from(mode: TakeoverMode) -> Self {
        match mode {
            TakeoverMode::Manual => Self::Manual,
            TakeoverMode::Hold => Self::Hold,
            TakeoverMode::Toggle => Self::Toggle,
        }
    }
}
impl From<Mode> for TakeoverMode {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Manual => TakeoverMode::Manual,
            Mode::Hold => TakeoverMode::Hold,
            Mode::Toggle => TakeoverMode::Toggle,
        }
    }
}

/// The Touchpad area choice. Deliberately not `mapping::PadFit`: its enum order
/// (`KeepShape`, `Stretch`, `FillArea`, `Portion`) differs from the combo order
/// ("Force proportions", "Full", "Portion…"), so neither discriminant is safe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    FillArea,
    Stretch,
    Portion,
}
impl Fit {
    /// Position in the Touchpad area combo.
    pub fn selection(self) -> usize {
        match self {
            Self::FillArea => 0,
            Self::Stretch => 1,
            Self::Portion => 2,
        }
    }

    pub fn from_selection(index: usize) -> Self {
        match index {
            1 => Self::Stretch,
            2 => Self::Portion,
            _ => Self::FillArea,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Desktop,
    Monitor,
    ScreenPortion,
    Window,
    TestArea,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Target {
    pub kind: TargetKind,
    /// Display device name (`\\.\DISPLAY2`); stable across refreshes, unlike an
    /// index. Absent for every kind but `Monitor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<String>,
}
impl Target {
    pub fn kind(kind: TargetKind) -> Self {
        Self {
            kind,
            monitor: None,
        }
    }
}

/// A screen rectangle in physical virtual-desktop pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}
impl From<RECT> for Rect {
    fn from(r: RECT) -> Self {
        Self {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        }
    }
}
impl From<Rect> for RECT {
    fn from(r: Rect) -> Self {
        Self {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        }
    }
}

/// A rectangle relative to a client area or the touchpad, 0.0..=1.0.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
pub struct Area {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}
impl From<NormRect> for Area {
    fn from(area: NormRect) -> Self {
        Self {
            left: area.left,
            top: area.top,
            right: area.right,
            bottom: area.bottom,
        }
    }
}
impl Area {
    /// The validated rectangle, or `None` when the stored values are unusable
    /// (hand-edited, or from a different display arrangement).
    pub fn norm_rect(self) -> Option<NormRect> {
        NormRect {
            left: self.left,
            top: self.top,
            right: self.right,
            bottom: self.bottom,
        }
        .validate()
        .ok()
    }
}

pub fn path(root: &Path) -> PathBuf {
    root.join("settings.json")
}

fn settings_path() -> Result<PathBuf, String> {
    gesture_backend::appdata_root().map(|root| path(&root))
}

/// The stored settings, or `None` when there is no usable file. Never fails the
/// launch: defaults stay in place for a missing, unreadable, stale or corrupt
/// file.
pub fn load() -> Option<Settings> {
    load_from(&settings_path().ok()?)
}

pub fn save(settings: &Settings) {
    if let Ok(path) = settings_path() {
        let _ = save_to(&path, settings);
    }
}

fn load_from(path: &Path) -> Option<Settings> {
    let settings: Settings = gesture_backend::read_json(path).ok()?;
    (settings.format == FORMAT).then_some(settings)
}

/// Atomic publish through the recovery journal's temp-file + rename writer. The
/// parent directory is created on first save.
fn save_to(path: &Path, settings: &Settings) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    gesture_backend::write_json(path, settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn scratch(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "pad2touch-settings-{name}-{}-{stamp}",
                std::process::id()
            ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn full() -> Settings {
        Settings {
            format: FORMAT,
            device: Some(r"\\?\HID#VID_05AC&PID_0277".into()),
            inject: Some(false),
            touch_feedback: Some(true),
            suppress_touchpad: Some(true),
            suppress_touchscreen: Some(false),
            start_mode: Some(Mode::Toggle),
            shortcut: Some(0x77),
            target: Some(Target {
                kind: TargetKind::Monitor,
                monitor: Some(r"\\.\DISPLAY2".into()),
            }),
            screen_rect: Some(Rect {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            }),
            window_area: Some(Area {
                left: 0.25,
                top: 0.25,
                right: 0.75,
                bottom: 0.75,
            }),
            pad_area: Some(Area {
                left: 0.0,
                top: 0.0,
                right: 1.0,
                bottom: 1.0,
            }),
            pad_fit: Some(Fit::Portion),
        }
    }

    #[test]
    fn the_settings_file_sits_beside_the_recovery_journals() {
        let root = Path::new(r"C:\Users\a\AppData\Local\Pad2Touch");
        let file = path(root);
        assert_eq!(file.file_name().unwrap(), "settings.json");
        assert_eq!(file.parent(), Some(root));
    }

    #[test]
    fn a_full_snapshot_round_trips() {
        let dir = scratch("roundtrip");
        let file = path(&dir);
        let original = full();
        save_to(&file, &original).unwrap();
        assert_eq!(load_from(&file), Some(original));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unset_fields_stay_absent_instead_of_being_written_as_null() {
        let dir = scratch("sparse");
        let file = path(&dir);
        let sparse = Settings {
            format: FORMAT,
            inject: Some(true),
            ..Settings::default()
        };
        save_to(&file, &sparse).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        assert!(!text.contains("null"), "{text}");
        assert_eq!(load_from(&file), Some(sparse));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_and_future_fields_are_tolerated() {
        let parsed: Settings = serde_json::from_str(r#"{"format":1,"later_feature":"x"}"#).unwrap();
        assert_eq!(
            parsed,
            Settings {
                format: FORMAT,
                ..Settings::default()
            }
        );
        let partial: Settings = serde_json::from_str(r#"{"format":1,"inject":false}"#).unwrap();
        assert_eq!(partial.inject, Some(false));
        assert_eq!(partial.pad_fit, None);
    }

    #[test]
    fn a_corrupt_or_stale_file_leaves_the_defaults_in_place() {
        let dir = scratch("corrupt");
        let file = path(&dir);
        fs::write(&file, b"{ not json").unwrap();
        assert_eq!(load_from(&file), None);
        fs::write(&file, br#"{"format":99,"inject":false}"#).unwrap();
        assert_eq!(load_from(&file), None);
        assert_eq!(load_from(&path(&dir.join("missing"))), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_first_save_creates_the_directory_and_leaves_no_temporary_file() {
        let dir = scratch("create");
        let nested = dir.join("nested");
        let file = path(&nested);
        save_to(&file, &full()).unwrap();
        assert!(file.is_file());
        let leftovers: Vec<String> = fs::read_dir(&nested)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn touchpad_area_names_do_not_follow_the_mapping_enum_order() {
        assert_eq!(Fit::FillArea.selection(), 0);
        assert_eq!(Fit::Stretch.selection(), 1);
        assert_eq!(Fit::Portion.selection(), 2);
        for index in 0..=2 {
            assert_eq!(Fit::from_selection(index).selection(), index);
        }
        assert_eq!(Fit::from_selection(usize::MAX), Fit::FillArea);
    }

    #[test]
    fn unusable_areas_are_rejected_instead_of_restored() {
        let good = Area {
            left: 0.25,
            top: 0.5,
            right: 0.75,
            bottom: 1.0,
        };
        assert!(good.norm_rect().is_some());
        for bad in [
            Area {
                left: -0.1,
                top: 0.0,
                right: 1.0,
                bottom: 1.0,
            },
            Area {
                left: 0.5,
                top: 0.0,
                right: 0.5,
                bottom: 1.0,
            },
            Area {
                left: f64::NAN,
                top: 0.0,
                right: 1.0,
                bottom: 1.0,
            },
        ] {
            assert!(bad.norm_rect().is_none(), "{bad:?}");
        }
    }
}
