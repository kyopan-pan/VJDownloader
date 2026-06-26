//! Syphon 出力。マスター映像を Syphon サーバとして公開し、VDMX などから利用できるようにする。
//!
//! `syphon` フィーチャー有効時のみコンパイルされ、公式 Syphon.framework（Metal 実装）を
//! 動的リンクする。フレームワークが見つからない、または初期化に失敗した場合は publisher を
//! 生成せず、出力は無効のまま安全に処理を継続する（呼び出し側で None として扱う）。
//!
//! ビルド要件:
//! - `third_party/Syphon.framework`（または環境変数 SYPHON_FRAMEWORK_DIR で指す場所）に
//!   公式 Syphon.framework を配置し、`cargo build --features syphon` する。
//! - 実行時はフレームワークが解決できる必要がある（.app では Contents/Frameworks へ同梱）。

use std::ffi::c_void;
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
    // Drop 順とライフタイム保持のために device も保持する。
    _device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    server: Retained<AnyObject>,
    texture: Retained<ProtocolObject<dyn MTLTexture>>,
    width: usize,
    height: usize,
}

impl SyphonPublisher {
    /// 指定解像度・名前で Syphon サーバを生成する。Metal/Syphon の初期化に失敗したら `None`。
    pub fn new(width: usize, height: usize, name: &str) -> Option<Self> {
        let device = MTLCreateSystemDefaultDevice()?;
        let queue = device.newCommandQueue()?;

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
        let texture = device.newTextureWithDescriptor(&desc)?;

        // Syphon.framework のクラスを実行時に解決（リンクされていなければ None）。
        let cls = AnyClass::get(c"SyphonMetalServer")?;
        let ns_name = NSString::from_str(name);
        let server: Retained<AnyObject> = unsafe {
            let alloc: Allocated<AnyObject> = msg_send![cls, alloc];
            msg_send![
                alloc,
                initWithName: &*ns_name,
                device: &*device,
                options: Option::<&AnyObject>::None,
            ]
        };

        Some(Self {
            _device: device,
            queue,
            server,
            texture,
            width,
            height,
        })
    }

    /// BGRA8（`width * height * 4` バイト）のマスター画像を 1 フレーム公開する。
    pub fn publish(&self, bgra: &[u8]) {
        if bgra.len() < self.width * self.height * 4 {
            return;
        }

        objc2::rc::autoreleasepool(|_| {
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
                return;
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
                    flipped: false,
                ];
            }
            cmd.commit();
        });
    }
}

impl Drop for SyphonPublisher {
    fn drop(&mut self) {
        unsafe {
            let _: () = msg_send![&*self.server, stop];
        }
    }
}
