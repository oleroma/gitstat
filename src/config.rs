//! `config.json` next to the executable: which folder to watch, how often, and
//! which parts of the UI are on.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "config.json";

/// Work-area corner of the primary monitor that holds the overlay badge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    #[default]
    BottomRight,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayConfig {
    pub enabled: bool,
    pub corner: Corner,
    /// Diameter in logical px.
    pub size: f32,
    /// Distance from the work-area edges in logical px.
    pub margin: f32,
    pub hide_when_synced: bool,
    pub hide_in_fullscreen: bool,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            corner: Corner::default(),
            size: 44.0,
            margin: 16.0,
            hide_when_synced: false,
            hide_in_fullscreen: true,
        }
    }
}

/// `PartialEq` lets the app ignore reloads of its own writes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Parent folder whose direct subfolders are the repos to watch.
    pub path: String,
    pub interval_minutes: f64,
    /// Event-driven local status; false checks on the interval only.
    pub watch: bool,
    /// Tray left-click opens the flyout panel.
    pub panel: bool,
    /// The normal status window is available.
    pub window: bool,
    pub overlay: OverlayConfig,
    /// desktop.ini status icons on the repo folders.
    pub folder_icons: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            path: "~/dev".into(),
            interval_minutes: 5.0,
            watch: true,
            panel: true,
            window: true,
            overlay: OverlayConfig::default(),
            folder_icons: false,
        }
    }
}

/// What a click on the tray icon or the overlay opens.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickAction {
    Panel,
    Window,
    Menu,
}

impl Config {
    /// Reads `file`, or creates it with defaults when it doesn't exist. A new
    /// config watches the folder that holds gitstat's own folder, so dropping
    /// gitstat into `D:\dev\gitstat` watches `D:\dev`.
    pub fn load_or_create(file: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(file) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", file.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut config = Config::default();
                if let Some(grandparent) = file.parent().and_then(Path::parent) {
                    config.path = grandparent.to_string_lossy().replace('\\', "/");
                }
                let text = serde_json::to_string_pretty(&config).unwrap_or_default();
                // Not being able to write it is fine; the defaults still work.
                let _ = std::fs::write(file, text + "\n");
                Ok(config)
            }
            Err(e) => Err(format!("{}: {e}", file.display())),
        }
    }

    /// `path` with `~` and `%VAR%` / `$VAR` expanded.
    pub fn repos_dir(&self) -> PathBuf {
        PathBuf::from(expand(&self.path))
    }

    /// The fetch sweep period; the Linux binary only runs `--once`.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs_f64(clamp_minutes(self.interval_minutes) * 60.0)
    }
}

/// Settings only the tray app reads; `--once` (the whole Linux binary) never does.
#[cfg_attr(not(windows), allow(dead_code))]
impl Config {
    /// Writes `file` through a temp file and a rename, so a crash never
    /// leaves half a config. Refuses to overwrite a file that doesn't parse:
    /// that is a hand edit in progress.
    pub fn save(&self, file: &Path) -> Result<(), String> {
        match std::fs::read_to_string(file) {
            Ok(text) => {
                if let Err(e) = serde_json::from_str::<Config>(&text) {
                    return Err(format!("{}: {e}; not overwriting it", file.display()));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            // Locked or unreadable: we can't tell whether it holds an edit, so leave it.
            Err(e) => return Err(format!("{}: {e}", file.display())),
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())? + "\n";
        let tmp = file.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, file).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{}: {e}", file.display())
        })
    }

    /// The panel, else the window, else the menu, so a click always does something.
    pub fn left_click(&self) -> ClickAction {
        if self.panel {
            ClickAction::Panel
        } else if self.window {
            ClickAction::Window
        } else {
            ClickAction::Menu
        }
    }

    /// Like `left_click`, but the overlay has no menu to fall back to.
    pub fn overlay_click(&self) -> Option<ClickAction> {
        match self.left_click() {
            ClickAction::Menu => None,
            action => Some(action),
        }
    }

    pub fn overlay_size(&self) -> f32 {
        clamp_f32(
            self.overlay.size,
            24.0,
            128.0,
            OverlayConfig::default().size,
        )
    }

    pub fn overlay_margin(&self) -> f32 {
        clamp_f32(
            self.overlay.margin,
            0.0,
            200.0,
            OverlayConfig::default().margin,
        )
    }
}

fn clamp_f32(value: f32, min: f32, max: f32, default: f32) -> f32 {
    if value.is_nan() {
        default
    } else {
        value.clamp(min, max)
    }
}

/// 1 minute to 1 day; NaN and junk fall back to the default.
#[cfg_attr(not(windows), allow(dead_code))]
fn clamp_minutes(minutes: f64) -> f64 {
    if minutes.is_finite() {
        minutes.clamp(1.0, 1440.0)
    } else {
        Config::default().interval_minutes
    }
}

/// Default location: `config.json` beside the executable.
pub fn default_file() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(FILE_NAME)))
        .unwrap_or_else(|| PathBuf::from(FILE_NAME))
}

/// Where generated icons and the folder-icon ledger live: `%LOCALAPPDATA%\gitstat`.
/// Never the exe dir, which usually sits inside the watched root.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn data_dir() -> PathBuf {
    data_dir_in(std::env::var_os("LOCALAPPDATA"))
}

fn data_dir_in(local_app_data: Option<std::ffi::OsString>) -> PathBuf {
    local_app_data
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("gitstat")
}

fn home() -> Option<String> {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()
}

fn expand(path: &str) -> String {
    let mut out = String::new();
    let mut rest = path;
    if (rest == "~" || rest.starts_with("~/") || rest.starts_with("~\\"))
        && let Some(home) = home()
    {
        out.push_str(&home);
        rest = &rest[1..];
    }
    while let Some(start) = rest.find(['%', '$']) {
        out.push_str(&rest[..start]);
        let sigil = rest.as_bytes()[start];
        let after = &rest[start + 1..];
        let (name, tail) = if sigil == b'%' {
            match after.find('%') {
                Some(end) => (&after[..end], &after[end + 1..]),
                None => ("", after),
            }
        } else {
            let end = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            (&after[..end], &after[end..])
        };
        match std::env::var(name) {
            Ok(value) if !name.is_empty() => out.push_str(&value),
            _ => out.push_str(&rest[start..rest.len() - tail.len()]),
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_home_and_variables() {
        // SAFETY: tests in this module don't read these variables concurrently.
        unsafe {
            std::env::set_var("HOME", "/home/me");
            std::env::remove_var("USERPROFILE");
            std::env::set_var("GITSTAT_TEST", "x");
        }
        assert_eq!(expand("~/dev"), "/home/me/dev");
        assert_eq!(expand("D:/dev"), "D:/dev");
        assert_eq!(expand("%GITSTAT_TEST%/a"), "x/a");
        assert_eq!(expand("$GITSTAT_TEST/a"), "x/a");
        assert_eq!(expand("%NOPE_NOT_SET%/a"), "%NOPE_NOT_SET%/a");
        assert_eq!(expand("100%"), "100%");
    }

    #[test]
    fn new_config_watches_the_folder_above_gitstat() {
        let dir = std::env::temp_dir().join(format!("gitstat-cfg-{}", std::process::id()));
        let app = dir.join("gitstat");
        std::fs::create_dir_all(&app).unwrap();
        let file = app.join(FILE_NAME);
        let _ = std::fs::remove_file(&file);
        let config = Config::load_or_create(&file).unwrap();
        assert_eq!(config.path, dir.to_string_lossy().replace('\\', "/"));
        assert!(file.exists());
        let again = Config::load_or_create(&file).unwrap();
        assert_eq!(again.path, config.path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fresh, empty temp folder per test, so parallel tests never share files.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gitstat-cfg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn old_two_key_file_loads_with_defaults() {
        let dir = temp_dir("old");
        let file = dir.join(FILE_NAME);
        std::fs::write(&file, r#"{ "path": "D:/dev", "interval_minutes": 10 }"#).unwrap();
        let config = Config::load_or_create(&file).unwrap();
        assert_eq!(
            config,
            Config {
                path: "D:/dev".into(),
                interval_minutes: 10.0,
                ..Config::default()
            }
        );
        assert!(config.watch && config.panel && config.window && !config.folder_icons);
        assert_eq!(config.overlay, OverlayConfig::default());
        assert!(!config.overlay.enabled && config.overlay.hide_in_fullscreen);
        // A partial overlay section keeps the other overlay defaults.
        std::fs::write(&file, r#"{ "overlay": { "enabled": true } }"#).unwrap();
        let config = Config::load_or_create(&file).unwrap();
        assert!(config.overlay.enabled);
        assert_eq!(config.overlay.size, 44.0);
        assert_eq!(config.overlay.corner, Corner::BottomRight);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corner_is_kebab_case() {
        for (corner, text) in [
            (Corner::TopLeft, "\"top-left\""),
            (Corner::TopRight, "\"top-right\""),
            (Corner::BottomLeft, "\"bottom-left\""),
            (Corner::BottomRight, "\"bottom-right\""),
        ] {
            assert_eq!(serde_json::to_string(&corner).unwrap(), text);
            assert_eq!(serde_json::from_str::<Corner>(text).unwrap(), corner);
        }
        assert!(serde_json::from_str::<Corner>("\"BottomRight\"").is_err());
    }

    #[test]
    fn overlay_size_and_margin_are_clamped() {
        let with = |size: f32, margin: f32| {
            let mut config = Config::default();
            config.overlay.size = size;
            config.overlay.margin = margin;
            (config.overlay_size(), config.overlay_margin())
        };
        assert_eq!(with(44.0, 16.0), (44.0, 16.0));
        assert_eq!(with(1.0, -5.0), (24.0, 0.0));
        assert_eq!(with(1000.0, 1000.0), (128.0, 200.0));
        assert_eq!(with(f32::INFINITY, f32::NEG_INFINITY), (128.0, 0.0));
        assert_eq!(with(f32::NAN, f32::NAN), (44.0, 16.0));
    }

    #[test]
    fn interval_is_clamped() {
        let minutes = |m: f64| {
            Config {
                interval_minutes: m,
                ..Config::default()
            }
            .interval()
            .as_secs_f64()
                / 60.0
        };
        assert_eq!(minutes(5.0), 5.0);
        assert_eq!(minutes(0.0), 1.0);
        assert_eq!(minutes(1e9), 1440.0);
        assert_eq!(minutes(f64::NAN), 5.0);
    }

    #[test]
    fn click_matrix() {
        use ClickAction::*;
        for (panel, window, left, overlay) in [
            (true, true, Panel, Some(Panel)),
            (true, false, Panel, Some(Panel)),
            (false, true, Window, Some(Window)),
            (false, false, Menu, None),
        ] {
            let config = Config {
                panel,
                window,
                ..Config::default()
            };
            assert_eq!(config.left_click(), left, "panel={panel} window={window}");
            assert_eq!(
                config.overlay_click(),
                overlay,
                "panel={panel} window={window}"
            );
        }
    }

    #[test]
    fn save_round_trips_without_leaving_a_tmp() {
        let dir = temp_dir("save");
        let file = dir.join(FILE_NAME);
        let mut config = Config {
            path: "D:/dev".into(),
            ..Config::default()
        };
        config.overlay.corner = Corner::TopLeft;
        config.overlay.enabled = true;
        config.folder_icons = true;
        // Creates a missing file.
        config.save(&file).unwrap();
        assert_eq!(Config::load_or_create(&file).unwrap(), config);
        // Overwrites a valid one, and saving what was loaded is stable.
        config.watch = false;
        config.save(&file).unwrap();
        let loaded = Config::load_or_create(&file).unwrap();
        assert_eq!(loaded, config);
        let text = std::fs::read_to_string(&file).unwrap();
        loaded.save(&file).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
        assert!(text.contains("\"top-left\""));
        assert!(!dir.join("config.json.tmp").exists());
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, [FILE_NAME]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_refuses_to_overwrite_an_unparsable_file() {
        let dir = temp_dir("refuse");
        let file = dir.join(FILE_NAME);
        for broken in [r#"{ "path": "D:/dev", "#, "", r#"{ "watch": "yes" }"#] {
            std::fs::write(&file, broken).unwrap();
            assert!(Config::default().save(&file).is_err(), "{broken:?}");
            assert_eq!(std::fs::read_to_string(&file).unwrap(), broken);
            assert!(!dir.join("config.json.tmp").exists());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn data_dir_prefers_local_app_data() {
        assert_eq!(
            data_dir_in(Some("C:/Users/me/AppData/Local".into())),
            Path::new("C:/Users/me/AppData/Local").join("gitstat")
        );
        assert_eq!(
            data_dir_in(Some("".into())),
            std::env::temp_dir().join("gitstat")
        );
        assert_eq!(data_dir_in(None), std::env::temp_dir().join("gitstat"));
    }
}
