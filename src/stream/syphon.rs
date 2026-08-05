//! Syphon 出力。マスター映像を Syphon サーバとして公開し、VDMX などから利用できるようにする。
//!
//! `syphon` フィーチャー有効時のみコンパイルされ、公式 Syphon.framework（Metal 実装）を
//! 実行時に読み込む。フレームワークが解決できない場合はエラーを返す。
//! Metal/Syphon の初期化に失敗した場合はエラーを返し、呼び出し側で表示する。
//!
//! ビルド要件:
//! - `third_party/Syphon.framework`（または環境変数 SYPHON_FRAMEWORK_DIR で指す場所）に
//!   公式 Syphon.framework を配置する。
//! - .app では Contents/Frameworks/Syphon.framework に同梱する。

use std::cell::Cell;
use std::env;
use std::ffi::{CString, c_char, c_int, c_void};
use std::path::PathBuf;
use std::ptr::NonNull;

use objc2::msg_send;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, ProtocolObject};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::NSString;
use objc2_metal::{
    MTLCommandBuffer, MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice, MTLOrigin,
    MTLPixelFormat, MTLRegion, MTLSize, MTLStorageMode, MTLTexture, MTLTextureDescriptor,
    MTLTextureUsage,
};

/// マスター映像を公開する Syphon Metal サーバ。
pub struct SyphonPublisher {
    // Syphon.framework の Objective-C クラスをプロセス内に保持する。
    _framework: NonNull<c_void>,
    // Drop 順とライフタイム保持のために device も保持する。
    _device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    server: Retained<AnyObject>,
    texture: Retained<ProtocolObject<dyn MTLTexture>>,
    width: usize,
    height: usize,
    published_frames: Cell<u64>,
}

impl SyphonPublisher {
    /// 指定解像度・名前で Syphon サーバを生成する。
    pub fn new(width: usize, height: usize, name: &str) -> Result<Self, String> {
        let framework = load_syphon_framework()?;
        let device =
            MTLCreateSystemDefaultDevice().ok_or("Metal デバイスを取得できませんでした。")?;
        let queue = device
            .newCommandQueue()
            .ok_or("Metal コマンドキューを作成できませんでした。")?;

        // CPU から書き込む BGRA8・共有ストレージのテクスチャ。
        let desc = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::BGRA8Unorm,
                width,
                height,
                false,
            )
        };
        desc.setUsage(MTLTextureUsage::ShaderRead);
        desc.setStorageMode(MTLStorageMode::Shared);
        let texture = device
            .newTextureWithDescriptor(&desc)
            .ok_or("Syphon 送信用 Metal テクスチャを作成できませんでした。")?;

        // Syphon.framework のクラスを実行時に解決する。
        let cls = AnyClass::get(c"SyphonMetalServer")
            .ok_or("SyphonMetalServer クラスを解決できませんでした。")?;
        let ns_name = NSString::from_str(name);
        let server: Option<Retained<AnyObject>> = unsafe {
            let alloc: Allocated<AnyObject> = msg_send![cls, alloc];
            msg_send![
                alloc,
                initWithName: &*ns_name,
                device: &*device,
                options: Option::<&AnyObject>::None,
            ]
        };
        let server = server.ok_or("Syphon Metal サーバの初期化に失敗しました。")?;

        Ok(Self {
            _framework: framework,
            _device: device,
            queue,
            server,
            texture,
            width,
            height,
            published_frames: Cell::new(0),
        })
    }

    /// BGRA8（`width * height * 4` バイト）のマスター画像を 1 フレーム公開する。
    pub fn publish(&self, bgra: &[u8]) -> bool {
        if bgra.len() < self.width * self.height * 4 {
            return false;
        }

        let published = objc2::rc::autoreleasepool(|_| {
            // CPU 側の BGRA をテクスチャへ転送。
            let region = MTLRegion {
                origin: MTLOrigin { x: 0, y: 0, z: 0 },
                size: MTLSize {
                    width: self.width,
                    height: self.height,
                    depth: 1,
                },
            };
            unsafe {
                self.texture
                    .replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                        region,
                        0,
                        NonNull::new(bgra.as_ptr() as *mut c_void).unwrap(),
                        self.width * 4,
                    );
            }

            let Some(cmd) = self.queue.commandBuffer() else {
                return false;
            };
            let rect = CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: self.width as f64,
                    height: self.height as f64,
                },
            };
            unsafe {
                let _: () = msg_send![
                    &*self.server,
                    publishFrameTexture: &*self.texture,
                    onCommandBuffer: &*cmd,
                    imageRegion: rect,
                    flipped: true,
                ];
            }
            cmd.commit();
            true
        });
        if published {
            self.published_frames
                .set(self.published_frames.get().saturating_add(1));
        }
        published
    }

    pub fn published_frames(&self) -> u64 {
        self.published_frames.get()
    }

    pub fn has_clients(&self) -> bool {
        unsafe { msg_send![&*self.server, hasClients] }
    }
}

const RTLD_NOW: c_int = 0x2;
const RTLD_LOCAL: c_int = 0x4;

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlerror() -> *const c_char;
}

fn load_syphon_framework() -> Result<NonNull<c_void>, String> {
    if AnyClass::get(c"SyphonMetalServer").is_some() {
        // クラスが既にロード済みなら、シンボル解決目的のハンドルは main program で十分。
        let handle = unsafe { dlopen(std::ptr::null(), RTLD_NOW | RTLD_LOCAL) };
        return NonNull::new(handle).ok_or_else(last_dlopen_error);
    }

    let mut errors = Vec::new();
    for path in syphon_framework_candidates() {
        let c_path = match CString::new(path.to_string_lossy().as_bytes()) {
            Ok(c_path) => c_path,
            Err(_) => continue,
        };
        let handle = unsafe { dlopen(c_path.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
        if let Some(handle) = NonNull::new(handle) {
            println!("[syphon] loaded framework: {}", path.to_string_lossy());
            return Ok(handle);
        }
        errors.push(format!(
            "{} ({})",
            path.to_string_lossy(),
            last_dlopen_error()
        ));
    }

    Err(format!(
        "Syphon.framework を読み込めませんでした。third_party/Syphon.framework または SYPHON_FRAMEWORK_DIR/SYPHON_FRAMEWORK_PATH を確認してください。試行: {}",
        errors.join(" / ")
    ))
}

fn syphon_framework_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(path) = env::var("SYPHON_FRAMEWORK_PATH") {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(dir) = env::var("SYPHON_FRAMEWORK_DIR") {
        candidates.push(PathBuf::from(dir).join("Syphon.framework").join("Syphon"));
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(mac_os_dir) = exe.parent() {
            candidates.push(
                mac_os_dir
                    .join("..")
                    .join("Frameworks")
                    .join("Syphon.framework")
                    .join("Syphon"),
            );
            candidates.push(mac_os_dir.join("Syphon.framework").join("Syphon"));
        }
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("third_party")
            .join("Syphon.framework")
            .join("Syphon"),
    );
    candidates.push(PathBuf::from("/Library/Frameworks/Syphon.framework/Syphon"));
    candidates.push(PathBuf::from(
        "/Applications/VDMX6.app/Contents/Frameworks/Syphon.framework/Syphon",
    ));
    candidates.push(PathBuf::from(
        "/Applications/VDMX5.app/Contents/Frameworks/Syphon.framework/Syphon",
    ));
    candidates
}

fn last_dlopen_error() -> String {
    unsafe {
        let err = dlerror();
        if err.is_null() {
            return "unknown dlopen error".to_string();
        }
        std::ffi::CStr::from_ptr(err).to_string_lossy().to_string()
    }
}

impl SyphonPublisher {
    pub fn server_name() -> &'static str {
        "VJDownloader Master"
    }
}

impl Drop for SyphonPublisher {
    fn drop(&mut self) {
        unsafe {
            let _: () = msg_send![&*self.server, stop];
        }
    }
}
