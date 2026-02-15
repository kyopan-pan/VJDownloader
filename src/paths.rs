use std::path::PathBuf;

pub fn default_download_dir() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join("Movies").join("VJDL")
}

pub fn app_data_dir() -> PathBuf {
    settings_dir()
}

pub fn settings_dir() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".vjdownloader")
}

pub fn settings_file_path() -> PathBuf {
    settings_dir().join("settings.properties")
}

pub fn search_index_db_path() -> PathBuf {
    app_data_dir().join("search_index.sqlite3")
}

pub fn make_absolute_path(raw: &str) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        return path;
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(path)
}

pub fn bin_dir() -> PathBuf {
    app_data_dir().join("bin")
}

fn resolve_tool_path(file_name: &str) -> PathBuf {
    let primary = app_data_dir().join("bin").join(file_name);
    if primary.exists() {
        return primary;
    }

    bin_dir().join(file_name)
}

pub fn yt_dlp_path() -> PathBuf {
    resolve_tool_path("yt-dlp")
}

pub fn ffmpeg_path() -> PathBuf {
    resolve_tool_path("ffmpeg")
}

pub fn ffprobe_path() -> PathBuf {
    resolve_tool_path("ffprobe")
}

pub fn deno_path() -> PathBuf {
    resolve_tool_path("deno")
}
