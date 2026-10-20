//! `config.json` next to the executable: which folder to watch and how often.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "config.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Parent folder whose direct subfolders are the repos to watch.
    pub path: String,
    pub interval_minutes: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            path: "~/dev".into(),
            interval_minutes: 5.0,
        }
    }
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

    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs_f64(clamp_minutes(self.interval_minutes) * 60.0)
    }
}

/// 1 minute to 1 day; NaN and junk fall back to the default.
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
}
