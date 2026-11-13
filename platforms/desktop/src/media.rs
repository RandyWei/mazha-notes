//! macOS 音视频播放：AVPlayer + AVPlayerItemVideoOutput 取帧，交给 GPUI 自绘。
//!
//! GPUI 没有原生视频控件，且 `PlatformWindow` 未暴露 NSView 句柄，无法叠加
//! AVPlayerLayer；因此视频帧经 CVPixelBuffer 复制成 BGRA `RenderImage` 后由
//! `paint_image` 绘制。音频只创建 AVPlayer，不建视频输出。

use std::path::Path;
use std::sync::Arc;

use gpui_kit::{Pixels, RenderImage, Size, px};
use mazha_md::MediaKind;

/// 播放器状态（平台无关外壳）。
pub struct MediaPlayer {
    playing: bool,
    position: f64,
    duration: f64,
    frame: Option<Arc<RenderImage>>,
    frame_size: Option<Size<Pixels>>,
    error: Option<String>,
    loading: bool,
    started: bool,
    #[cfg(target_os = "macos")]
    inner: Option<MacPlayer>,
}

impl MediaPlayer {
    /// 创建播放器。`src` 可为 http(s)、协议相对 URL 或相对/绝对本地路径。
    ///
    /// 网络地址直接交给 AVPlayer，创建时不发起播放。
    pub fn open(kind: MediaKind, src: &str, doc_path: Option<&Path>) -> Result<Self, String> {
        #[cfg(target_os = "macos")]
        {
            let inner = MacPlayer::new(kind, src, doc_path)?;
            Ok(Self {
                playing: false,
                position: 0.0,
                duration: 0.0,
                frame: None,
                frame_size: None,
                error: None,
                loading: true,
                started: false,
                inner: Some(inner),
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (kind, src, doc_path);
            Err("当前平台暂不支持音视频播放".to_string())
        }
    }

    /// 是否正在播放。
    pub fn playing(&self) -> bool {
        self.playing
    }

    /// 当前播放位置（秒）。
    pub fn position(&self) -> f64 {
        self.position
    }

    /// 总时长（秒；未知/直播为 0）。
    pub fn duration(&self) -> f64 {
        self.duration
    }

    /// 最新视频帧（音频始终为 `None`）。
    pub fn frame(&self) -> Option<&Arc<RenderImage>> {
        self.frame.as_ref()
    }

    /// 最新视频帧的自然尺寸。
    pub fn frame_size(&self) -> Option<Size<Pixels>> {
        self.frame_size
    }

    /// 播放错误。
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// 是否正在等待媒体加载或网络缓冲。
    pub fn loading(&self) -> bool {
        self.loading
    }

    /// 播放/暂停；播放结束后再次播放会回到开头。
    pub fn toggle(&mut self) {
        #[cfg(target_os = "macos")]
        if self.inner.is_none() {
            return;
        }
        if self.playing {
            #[cfg(target_os = "macos")]
            if let Some(inner) = self.inner.as_ref() {
                unsafe {
                    inner.player.pause();
                }
            }
            self.playing = false;
            self.loading = false;
            self.started = false;
            return;
        }
        if self.duration > 0.0 && self.position >= self.duration - 0.5 {
            self.seek_fraction(0.0);
        }
        #[cfg(target_os = "macos")]
        if let Some(inner) = self.inner.as_ref() {
            unsafe {
                inner.player.play();
            }
        }
        self.playing = true;
        self.loading = true;
        self.started = false;
    }

    /// 按进度（0..=1）跳转。
    pub fn seek_fraction(&mut self, fraction: f32) {
        if self.duration <= 0.0 || !self.duration.is_finite() {
            return;
        }
        let seconds = (fraction.clamp(0.0, 1.0) as f64 * self.duration).clamp(0.0, self.duration);
        #[cfg(target_os = "macos")]
        if let Some(inner) = self.inner.as_ref() {
            unsafe {
                inner.player.seekToTime(CMTime::with_seconds(seconds, 600));
            }
        }
        self.position = seconds;
    }

    /// 轮询播放进度与视频帧；返回状态是否有可见变化。
    pub fn poll(&mut self) -> bool {
        if self.error.is_some() {
            return false;
        }
        let mut changed = false;
        #[cfg(target_os = "macos")]
        if let Some(inner) = self.inner.as_ref() {
            let item = &inner.item;
            let status = unsafe { item.status() };
            if status == AVPlayerItemStatus::Failed {
                self.error = Some("媒体加载失败".to_string());
                self.playing = false;
                self.loading = false;
                self.started = false;
                return true;
            }
            if status != AVPlayerItemStatus::Unknown {
                let duration = seconds(unsafe { item.duration() });
                if duration > 0.0 && (duration - self.duration).abs() > 0.01 {
                    self.duration = duration;
                    changed = true;
                }
            }
            let position = seconds(unsafe { inner.player.currentTime() });
            if (position - self.position).abs() > 0.001 {
                self.position = position;
                changed = true;
            }
            let control_status = unsafe { inner.player.timeControlStatus() };
            if control_status == AVPlayerTimeControlStatus::Playing {
                if self.loading || !self.started {
                    changed = true;
                }
                self.loading = false;
                self.started = true;
            } else if control_status == AVPlayerTimeControlStatus::WaitingToPlayAtSpecifiedRate {
                if !self.loading {
                    changed = true;
                }
                self.loading = self.playing;
            }
            if let Some(output) = inner.output.as_ref() {
                let time = unsafe { inner.player.currentTime() };
                if unsafe { output.hasNewPixelBufferForItemTime(time) } {
                    let mut display_time = unsafe { CMTime::with_seconds(0.0, 600) };
                    if let Some(buffer) = unsafe {
                        output
                            .copyPixelBufferForItemTime_itemTimeForDisplay(time, &mut display_time)
                    } && let Some((frame, size)) = pixel_buffer_frame(&buffer)
                    {
                        self.frame = Some(frame);
                        self.frame_size = Some(size);
                        changed = true;
                    }
                }
            }
            let reached_duration = self.duration > 0.0 && self.position >= self.duration - 0.05;
            let player_paused = control_status == AVPlayerTimeControlStatus::Paused;
            if playback_ended(self.playing, self.started, reached_duration, player_paused) {
                unsafe {
                    inner.player.pause();
                    inner.player.seekToTime(CMTime::with_seconds(0.0, 600));
                }
                self.playing = false;
                self.loading = false;
                self.started = false;
                self.position = 0.0;
                changed = true;
            }
        }
        changed
    }
}

/// 播放结束判定：接近总时长，或已真正播放后 AVPlayer 自行回到 Paused。
fn playback_ended(
    playing: bool,
    started: bool,
    reached_duration: bool,
    player_paused: bool,
) -> bool {
    playing && (reached_duration || started && player_paused)
}

/// 常规时长读数：非有限/负值归 0。
fn seconds(time: CMTime) -> f64 {
    let s = unsafe { time.seconds() };
    if s.is_finite() && s > 0.0 { s } else { 0.0 }
}

#[cfg(target_os = "macos")]
struct MacPlayer {
    player: objc2::rc::Retained<AVPlayer>,
    item: objc2::rc::Retained<AVPlayerItem>,
    output: Option<objc2::rc::Retained<AVPlayerItemVideoOutput>>,
}

#[cfg(target_os = "macos")]
impl MacPlayer {
    fn new(kind: MediaKind, src: &str, doc_path: Option<&Path>) -> Result<Self, String> {
        use objc2::{AnyThread, MainThreadMarker};
        use objc2_av_foundation::{AVPlayer, AVPlayerItemVideoOutput};
        use objc2_core_video::kCVPixelFormatType_32BGRA;
        use objc2_foundation::{NSDictionary, NSNumber, NSString};

        let mtm = MainThreadMarker::new().ok_or_else(|| "必须在主线程创建播放器".to_string())?;
        let url = media_url(src, doc_path).ok_or_else(|| "无法解析媒体地址".to_string())?;
        let player = unsafe { AVPlayer::playerWithURL(&url, mtm) };
        let item = unsafe { player.currentItem() }.ok_or_else(|| "无法创建播放项".to_string())?;
        let output = if kind == MediaKind::Video {
            // 用字面量键避免 CFString → NSString 的桥接：CoreVideo 的键内容就是
            // `PixelFormatType`，NSDictionary 按字符串值比较键。
            let key = NSString::from_str("PixelFormatType");
            let value = NSNumber::numberWithUnsignedInt(kCVPixelFormatType_32BGRA);
            // AnyObject 是 ObjC 对象的透明视图；NSNumber 可直接按引用转换。
            let value_obj = unsafe { &*((&*value) as *const NSNumber).cast::<AnyObject>() };
            let attributes =
                NSDictionary::<NSString, AnyObject>::from_slices(&[&*key], &[value_obj]);
            let output = unsafe {
                AVPlayerItemVideoOutput::initWithPixelBufferAttributes(
                    AVPlayerItemVideoOutput::alloc(),
                    Some(&attributes),
                )
            };
            unsafe { item.addOutput(&output) };
            Some(output)
        } else {
            None
        };
        Ok(Self {
            player,
            item,
            output,
        })
    }
}

/// 地址解析：远程走 URL，本地路径转 file URL。
#[cfg(target_os = "macos")]
fn media_url(src: &str, doc_path: Option<&Path>) -> Option<objc2::rc::Retained<NSURL>> {
    use objc2_foundation::{NSString, NSURL};

    let src = src.trim();
    let lower = src.to_ascii_lowercase();
    let remote = if lower.starts_with("http://") || lower.starts_with("https://") {
        Some(src.to_string())
    } else if src.starts_with("//") {
        Some(format!("https:{src}"))
    } else {
        None
    };
    if let Some(url) = remote {
        return NSURL::URLWithString(&NSString::from_str(&url));
    }
    let path = crate::resolve_image_path(src, doc_path)?;
    Some(NSURL::fileURLWithPath(&NSString::from_str(
        &path.to_string_lossy(),
    )))
}

/// 把 32BGRA 的 CVPixelBuffer 复制成 RenderImage（内部同样按 BGRA 解释）。
#[cfg(target_os = "macos")]
fn pixel_buffer_frame(buffer: &CVPixelBuffer) -> Option<(Arc<RenderImage>, Size<Pixels>)> {
    use objc2_core_video::{
        CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
        CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
        CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress, kCVPixelFormatType_32BGRA,
    };

    if CVPixelBufferGetPixelFormatType(buffer) != kCVPixelFormatType_32BGRA {
        return None;
    }
    if unsafe { CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) } != 0 {
        return None;
    }
    let width = CVPixelBufferGetWidth(buffer);
    let height = CVPixelBufferGetHeight(buffer);
    let stride = CVPixelBufferGetBytesPerRow(buffer);
    let base = CVPixelBufferGetBaseAddress(buffer) as *const u8;
    if width == 0 || height == 0 || stride < width * 4 || base.is_null() {
        let _ = unsafe { CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) };
        return None;
    }
    let row_bytes = width * 4;
    let bytes = unsafe { std::slice::from_raw_parts(base, stride * height) };
    let mut pixels = Vec::with_capacity(row_bytes * height);
    for row in bytes.chunks_exact(stride) {
        pixels.extend_from_slice(&row[..row_bytes]);
    }
    let _ = unsafe { CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) };
    let image = image::RgbaImage::from_raw(width as u32, height as u32, pixels)?;
    let size = Size {
        width: px(width as f32),
        height: px(height as f32),
    };
    Some((
        Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(
            image
        )])),
        size,
    ))
}

#[cfg(target_os = "macos")]
use objc2::runtime::AnyObject;
#[cfg(target_os = "macos")]
use objc2_av_foundation::{
    AVPlayer, AVPlayerItem, AVPlayerItemStatus, AVPlayerItemVideoOutput, AVPlayerTimeControlStatus,
};
#[cfg(target_os = "macos")]
use objc2_core_media::CMTime;
#[cfg(target_os = "macos")]
use objc2_core_video::CVPixelBuffer;
#[cfg(target_os = "macos")]
use objc2_foundation::NSURL;

#[cfg(test)]
mod tests {
    use super::playback_ended;

    #[test]
    fn playback_end_requires_active_playback() {
        assert!(playback_ended(true, true, false, true));
        assert!(playback_ended(true, false, true, false));
        assert!(!playback_ended(false, true, true, true));
    }

    #[test]
    fn initial_buffering_pause_is_not_end() {
        assert!(!playback_ended(true, false, false, true));
    }
}
