use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::paths::{
    default_download_dir, legacy_settings_file_path, make_absolute_path, settings_file_path,
};

#[derive(Clone, Debug)]
pub struct SettingsData {
    pub window_width: String,
    pub window_height: String,
    pub download_panel_width: String,
    pub search_panel_width: String,
    pub download_dir: String,
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

pub fn load_cookie_args() -> Vec<String> {
    let props = load_settings_properties();
    let enabled = props
        .get("cookies.from_browser.enabled")
        .map(|v| parse_bool(v, false))
        .unwrap_or(false);
    if !enabled {
        return Vec::new();
    }
    let browser = props
        .get("cookies.from_browser.browser")
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let Some(browser) = browser else {
        return Vec::new();
    };
    let profile = props
        .get("cookies.from_browser.profile")
        .map(|v| v.trim().to_string())
        .unwrap_or_default();
    let value = if profile.is_empty() {
        browser
    } else {
        format!("{browser}:{profile}")
    };
    vec!["--cookies-from-browser".to_string(), value]
}

fn load_settings_properties() -> HashMap<String, String> {
    let path = settings_file_path();
    if let Some(props) = read_properties_from_path(&path) {
        return props;
    }
    read_properties_from_path(&legacy_settings_file_path()).unwrap_or_default()
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
