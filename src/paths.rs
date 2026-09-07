use std::path::PathBuf;
use std::sync::OnceLock;
#[cfg(target_os = "windows")]
use std::{fs::File, io::Read, process::Command};

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
    resolve_tool_path(&executable_name("yt-dlp"))
}

pub fn ffmpeg_path() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    cached_media_tool_path(&PATH, "ffmpeg")
}

pub fn ffprobe_path() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    cached_media_tool_path(&PATH, "ffprobe")
}

// 探索は外部プロセスの起動を伴うため、確定した結果だけを記憶する。
// 未発見のまま記憶すると、起動後にffmpegを導入しても再起動まで復旧できない。
fn cached_media_tool_path(cache: &OnceLock<PathBuf>, name: &str) -> PathBuf {
    if let Some(path) = cache.get() {
        return path.clone();
    }
    match resolve_media_tool_path(name) {
        Some(path) => cache.get_or_init(|| path).clone(),
        None => bin_dir().join(executable_name(name)),
    }
}

// ffmpegとffprobeの両方が使用可能かを返す。探索結果を記憶しないため、
// アプリ起動後に導入した場合でも最新の状態を判定できる。
#[cfg(target_os = "windows")]
pub fn media_tools_ready() -> bool {
    resolve_media_tool_path("ffmpeg").is_some() && resolve_media_tool_path("ffprobe").is_some()
}

pub fn deno_path() -> PathBuf {
    resolve_tool_path(&executable_name("deno"))
}

pub fn executable_name(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

// 使用できると確認できた場合のみ Some を返す。見つからない場合は呼び出し側で既定パスへ倒す。
fn resolve_media_tool_path(name: &str) -> Option<PathBuf> {
    let file_name = executable_name(name);
    let private = bin_dir().join(&file_name);

    #[cfg(target_os = "windows")]
    {
        // 誤って配置されたmacOS/LinuxバイナリをWindowsで起動しない。
        if is_usable_windows_media_tool(&private) {
            return Some(private);
        }

        if let Some(path_env) = std::env::var_os("PATH") {
            if let Some(path) = std::env::split_paths(&path_env)
                .map(|dir| dir.join(&file_name))
                .find(|path| is_usable_windows_media_tool(path))
            {
                return Some(path);
            }
        }

        None
    }

    #[cfg(not(target_os = "windows"))]
    private.exists().then_some(private)
}

#[cfg(target_os = "windows")]
fn is_usable_windows_media_tool(path: &std::path::Path) -> bool {
    let mut signature = [0_u8; 2];
    let has_pe_signature = File::open(path)
        .and_then(|mut file| file.read_exact(&mut signature))
        .is_ok()
        && signature == *b"MZ";
    has_pe_signature
        && Command::new(path)
            .arg("-version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
}
