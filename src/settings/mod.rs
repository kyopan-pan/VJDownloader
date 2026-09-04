pub mod ui;

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::download::DownloadMode;
use crate::paths::{default_download_dir, make_absolute_path, settings_file_path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChromeProfile {
    pub id: String,
    pub display_name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SettingsData {
    pub window_width: String,
    pub window_height: String,
    pub download_panel_width: String,
    pub search_panel_width: String,
    pub download_dir: String,
    pub download_mode: DownloadMode,
    pub search_roots: Vec<String>,
    pub cookies_enabled: bool,
    pub cookies_browser: String,
    pub cookies_profile: String,
}

impl SettingsData {
    pub fn load() -> Self {
        let props = load_settings_properties();
        let window_width = parse_dimension(
            props.get("window.width"),
            DEFAULT_WINDOW_WIDTH,
            MIN_WINDOW_WIDTH,
        );
        let window_height = parse_dimension(
            props.get("window.height"),
            DEFAULT_WINDOW_HEIGHT,
            MIN_WINDOW_HEIGHT,
        );
        let download_panel_width = parse_dimension(
            props.get("layout.download.width"),
            DEFAULT_MAIN_PANEL_WIDTH,
            MIN_MAIN_PANEL_WIDTH,
        );
        let search_panel_width = parse_dimension(
            props.get("layout.search.width"),
            DEFAULT_MAIN_PANEL_WIDTH,
            MIN_MAIN_PANEL_WIDTH,
        );
        let download_dir = props
            .get("download.dir")
            .map(|value| normalize_dir(value))
            .unwrap_or_else(default_download_dir)
            .to_string_lossy()
            .to_string();
        let download_mode = props
            .get("download.mode")
            .and_then(|value| DownloadMode::from_key(value))
            .unwrap_or_default();
        let search_roots = props
            .get("search.roots")
            .map(|value| decode_path_list(value))
            .unwrap_or_default()
            .into_iter()
            .map(|raw| normalize_dir(&raw).to_string_lossy().to_string())
            .collect();
        let cookies_enabled = props
            .get("cookies.from_browser.enabled")
            .map(|v| parse_bool(v, false))
            .unwrap_or(false);
        let cookies_browser = props
            .get("cookies.from_browser.browser")
            .map(|v| v.trim().to_string())
            .unwrap_or_default();
        let cookies_profile = props
            .get("cookies.from_browser.profile")
            .map(|v| v.trim().to_string())
            .unwrap_or_default();
        Self {
            window_width: format_dimension(window_width),
            window_height: format_dimension(window_height),
            download_panel_width: format_dimension(download_panel_width),
            search_panel_width: format_dimension(search_panel_width),
            download_dir,
            download_mode,
            search_roots,
            cookies_enabled,
            cookies_browser,
            cookies_profile,
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = settings_file_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        fs::write(path, self.to_properties_string()).map_err(|err| err.to_string())
    }

    fn to_properties_string(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!("window.width={}", self.window_width.trim()));
        lines.push(format!("window.height={}", self.window_height.trim()));
        lines.push(format!(
            "layout.download.width={}",
            self.download_panel_width.trim()
        ));
        lines.push(format!(
            "layout.search.width={}",
            self.search_panel_width.trim()
        ));
        let download_dir = self.download_dir.trim();
        lines.push(format!("download.dir={download_dir}"));
        lines.push(format!("download.mode={}", self.download_mode.as_key()));
        lines.push(format!(
            "search.roots={}",
            encode_path_list(&self.search_roots)
        ));
        lines.push(format!(
            "cookies.from_browser.enabled={}",
            if self.cookies_enabled {
                "true"
            } else {
                "false"
            }
        ));
        lines.push(format!(
            "cookies.from_browser.browser={}",
            self.cookies_browser.trim()
        ));
        lines.push(format!(
            "cookies.from_browser.profile={}",
            self.cookies_profile.trim()
        ));
        lines.join("\n")
    }
}

pub fn save_settings(data: &SettingsData) -> Result<(), String> {
    data.save()
}

pub fn cookie_args_from_settings(data: &SettingsData) -> Vec<String> {
    if !data.cookies_enabled {
        return Vec::new();
    }
    let browser = data.cookies_browser.trim();
    if browser.is_empty() {
        return Vec::new();
    }
    let profile = data.cookies_profile.trim();
    let value = if profile.is_empty() {
        browser.to_string()
    } else {
        format!("{browser}:{profile}")
    };
    vec!["--cookies-from-browser".to_string(), value]
}

pub fn load_chrome_profiles() -> Vec<ChromeProfile> {
    let Some(local_state_path) = chrome_local_state_path() else {
        return Vec::new();
    };
    read_chrome_profiles_from_local_state(&local_state_path)
}

fn chrome_local_state_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(
        home.join("Library")
            .join("Application Support")
            .join("Google")
            .join("Chrome")
            .join("Local State"),
    )
}

fn read_chrome_profiles_from_local_state(path: &PathBuf) -> Vec<ChromeProfile> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(_) => return Vec::new(),
    };
    let json: serde_json::Value = match serde_json::from_str(&contents) {
        Ok(json) => json,
        Err(_) => return Vec::new(),
    };
    let Some(info_cache) = json
        .get("profile")
        .and_then(|profile| profile.get("info_cache"))
        .and_then(|info_cache| info_cache.as_object())
    else {
        return Vec::new();
    };

    let mut profiles: Vec<_> = info_cache
        .iter()
        .filter_map(|(id, value)| {
            if id.trim().is_empty() {
                return None;
            }
            let display_name = chrome_profile_display_name(id, value);
            Some(ChromeProfile {
                id: id.to_string(),
                display_name,
            })
        })
        .collect();
    profiles.sort_by(|a, b| chrome_profile_sort_key(&a.id).cmp(&chrome_profile_sort_key(&b.id)));
    profiles
}

fn chrome_profile_display_name(id: &str, value: &serde_json::Value) -> String {
    for key in ["user_name", "gaia_name", "name"] {
        if let Some(name) = value.get(key).and_then(|name| name.as_str()) {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    id.to_string()
}

fn chrome_profile_sort_key(id: &str) -> (u8, u32, String) {
    if id == "Default" {
        return (0, 0, id.to_string());
    }
    if let Some(number) = id.strip_prefix("Profile ").and_then(|raw| raw.parse().ok()) {
        return (1, number, id.to_string());
    }
    (2, 0, id.to_string())
}

fn load_settings_properties() -> HashMap<String, String> {
    let path = settings_file_path();
    read_properties_from_path(&path).unwrap_or_default()
}

fn read_properties_from_path(path: &PathBuf) -> Option<HashMap<String, String>> {
    let mut props = HashMap::new();
    let contents = fs::read_to_string(path).ok()?;

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let mut split = line.splitn(2, |c| c == '=' || c == ':');
        let key = split.next().unwrap_or("").trim();
        let value = split.next().unwrap_or("").trim();
        if !key.is_empty() {
            props.insert(key.to_string(), value.to_string());
        }
    }
    Some(props)
}

fn parse_bool(raw: &str, fallback: bool) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return fallback;
    }
    trimmed.eq_ignore_ascii_case("true")
}

const DEFAULT_WINDOW_WIDTH: f32 = 860.0;
const DEFAULT_WINDOW_HEIGHT: f32 = 1000.0;
const MIN_WINDOW_WIDTH: f32 = 320.0;
const MIN_WINDOW_HEIGHT: f32 = 320.0;
const DEFAULT_MAIN_PANEL_WIDTH: f32 = 430.0;
const MIN_MAIN_PANEL_WIDTH: f32 = 1.0;

fn parse_dimension(raw: Option<&String>, fallback: f32, min: f32) -> f32 {
    let Some(raw) = raw else {
        return fallback.max(min);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return fallback.max(min);
    }
    let parsed = trimmed.parse::<f32>().unwrap_or(fallback);
    parsed.max(min)
}

fn format_dimension(value: f32) -> String {
    if value.fract() == 0.0 {
        format!("{:.0}", value)
    } else {
        format!("{value}")
    }
}

fn normalize_dir(value: &str) -> PathBuf {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return default_download_dir();
    }
    make_absolute_path(trimmed)
}

fn encode_path_list(paths: &[String]) -> String {
    let mut encoded = Vec::new();
    for path in paths {
        let mut escaped = String::new();
        for ch in path.chars() {
            match ch {
                '\\' => escaped.push_str("\\\\"),
                '|' => escaped.push_str("\\|"),
                _ => escaped.push(ch),
            }
        }
        encoded.push(escaped);
    }
    encoded.join("|")
}

fn decode_path_list(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut escape = false;
    for ch in raw.chars() {
        if escape {
            buf.push(ch);
            escape = false;
            continue;
        }
        match ch {
            '\\' => escape = true,
            '|' => {
                let trimmed = buf.trim();
                if !trimmed.is_empty() {
                    out.push(trimmed.to_string());
                }
                buf.clear();
            }
            _ => buf.push(ch),
        }
    }
    let trimmed = buf.trim();
    if !trimmed.is_empty() {
        out.push(trimmed.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_chrome_profiles_with_account_name_and_sorted_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Local State");
        fs::write(
            &path,
            r#"{
                "profile": {
                    "info_cache": {
                        "Profile 2": { "name": "Profile 2", "user_name": "work@example.com" },
                        "Default": { "name": "ユーザー 1" },
                        "Profile 1": { "name": "Profile 1", "gaia_name": "Personal" }
                    }
                }
            }"#,
        )
        .unwrap();

        let profiles = read_chrome_profiles_from_local_state(&path);

        assert_eq!(
            profiles,
            vec![
                ChromeProfile {
                    id: "Default".to_string(),
                    display_name: "ユーザー 1".to_string(),
                },
                ChromeProfile {
                    id: "Profile 1".to_string(),
                    display_name: "Personal".to_string(),
                },
                ChromeProfile {
                    id: "Profile 2".to_string(),
                    display_name: "work@example.com".to_string(),
                },
            ]
        );
    }

    #[test]
    fn ignores_missing_or_invalid_chrome_local_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Local State");

        assert!(read_chrome_profiles_from_local_state(&path).is_empty());

        fs::write(&path, "not json").unwrap();
        assert!(read_chrome_profiles_from_local_state(&path).is_empty());
    }
}
