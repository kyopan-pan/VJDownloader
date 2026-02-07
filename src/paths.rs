use std::path::PathBuf;

pub fn default_download_dir() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join("Movies").join("YtDlpDownloads")
}

pub fn app_data_dir() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".ytdownloader")
}

pub fn settings_file_path() -> PathBuf {
    app_data_dir().join("settings.properties")
}

pub fn bin_dir() -> PathBuf {
    app_data_dir().join("bin")
}

pub fn yt_dlp_path() -> PathBuf {
    bin_dir().join("yt-dlp")
}

pub fn ffmpeg_path() -> PathBuf {
    bin_dir().join("ffmpeg")
}

pub fn deno_path() -> PathBuf {
    bin_dir().join("deno")
}
