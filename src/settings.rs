use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::paths::settings_file_path;

pub fn load_download_dir_from_settings() -> Option<PathBuf> {
    let props = load_settings_properties();
    props
        .get("download.dir")
        .and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(PathBuf::from(trimmed))
            }
        })
        .map(|path| path)
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
    let mut props = HashMap::new();
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(_) => return props,
    };

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
    props
}

fn parse_bool(raw: &str, fallback: bool) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return fallback;
    }
    trimmed.eq_ignore_ascii_case("true")
}
