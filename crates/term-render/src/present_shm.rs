//! Presentation for software GL on X11.
//!
//! Mesa's EGL swrast path sends every swapped frame through the X socket (a whole-window
//! `PutImage`, about 2 MB at 80x24), which is most of the time between submit and pixels
//! on screen. When the server offers MIT-SHM, the frame is read straight into a shared
//! segment and shown with `ShmPutImage` instead, as GLX swrast (and Alacritty) does.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use glow::HasContext;
use wgpu::hal::api::Gles;
use wgpu::hal::gles::TextureInner;
use wgpu::rwh::RawWindowHandle;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::shm::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{self, ConnectionExt as _, ImageFormat, ImageOrder};
use x11rb::rust_connection::RustConnection;

pub struct ShmPresenter {
    conn: RustConnection,
    window: xproto::Window,
    gc: xproto::Gcontext,
    depth: u8,
    fbo: Option<glow::Framebuffer>,
    segment: Option<Segment>,
    /// A `ShmPutImage` the server has not reported complete, so the segment is still read.
    busy: bool,
}

struct Segment {
    id: shm::Seg,
    ptr: *mut u8,
    len: usize,
    size: (u32, u32),
}

impl Drop for Segment {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
    }
}

impl ShmPresenter {
    /// Returns `None` when the frame is not drawn by software GL into an X11 window.
    pub fn new(handle: RawWindowHandle, info: &wgpu::AdapterInfo) -> Option<Result<Self, String>> {
        let window = match handle {
            RawWindowHandle::Xlib(h) => u32::try_from(h.window).ok()?,
            RawWindowHandle::Xcb(h) => h.window.get(),
            _ => return None,
        };
        if info.backend != wgpu::Backend::Gl || info.device_type != wgpu::DeviceType::Cpu {
            return None;
        }
        Some(Self::connect(window))
    }

    fn connect(window: xproto::Window) -> Result<Self, String> {
        let (conn, _) = x11rb::connect(None).map_err(|e| e.to_string())?;
        if conn
            .extension_information(shm::X11_EXTENSION_NAME)
            .map_err(|e| e.to_string())?
            .is_none()
        {
            return Err("the X server has no MIT-SHM".into());
        }
        let version = conn
            .shm_query_version()
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        if (version.major_version, version.minor_version) < (1, 2) {
            return Err("MIT-SHM cannot pass file descriptors".into());
        }
        let geometry = conn
            .get_geometry(window)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        let visual = conn
            .get_window_attributes(window)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?
            .visual;
        let setup = conn.setup();
        let bits = setup
            .pixmap_formats
            .iter()
            .find(|f| f.depth == geometry.depth)
            .map(|f| f.bits_per_pixel);
        let masks = setup
            .roots
            .iter()
            .flat_map(|root| &root.allowed_depths)
            .flat_map(|depth| &depth.visuals)
            .find(|v| v.visual_id == visual)
            .map(|v| (v.red_mask, v.green_mask, v.blue_mask));
        if !bgra_layout(setup.image_byte_order, bits, masks) {
            return Err("the window's pixel layout is not BGRA".into());
        }
        let gc = conn.generate_id().map_err(|e| e.to_string())?;
        conn.create_gc(
            gc,
            window,
            &xproto::CreateGCAux::new().graphics_exposures(0),
        )
        .map_err(|e| e.to_string())?
        .check()
        .map_err(|e| e.to_string())?;
        Ok(ShmPresenter {
            conn,
            window,
            gc,
            depth: geometry.depth,
            fbo: None,
            segment: None,
            busy: false,
        })
    }

    /// Shows `texture`, a rendered `width` x `height` frame, in the window.
    pub fn present(
        &mut self,
        device: &wgpu::Device,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        let (w, h) = put_size(width, height).ok_or("frame too large for PutImage")?;
        self.wait_idle()?;
        if self
            .segment
            .as_ref()
            .is_none_or(|s| s.size != (width, height))
        {
            self.attach(width, height)?;
        }
        let segment = self.segment.as_ref().ok_or("no segment")?;
        let pixels = unsafe { std::slice::from_raw_parts_mut(segment.ptr, segment.len) };
        let device = unsafe { device.as_hal::<Gles>() }.ok_or("not a GL device")?;
        let texture = unsafe { texture.as_hal::<Gles>() }.ok_or("not a GL texture")?;
        let gl = device.context().lock();
        unsafe {
            let fbo = match self.fbo {
                Some(fbo) => fbo,
                None => *self.fbo.insert(gl.create_framebuffer()?),
            };
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(fbo));
            match texture.inner {
                TextureInner::Renderbuffer { raw } => gl.framebuffer_renderbuffer(
                    glow::READ_FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::RENDERBUFFER,
                    Some(raw),
                ),
                TextureInner::Texture { raw, .. } => gl.framebuffer_texture_2d(
                    glow::READ_FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::TEXTURE_2D,
                    Some(raw),
                    0,
                ),
                _ => {
                    gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
                    return Err("frame is not a GL texture".into());
                }
            }
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
            gl.pixel_store_i32(glow::PACK_ROW_LENGTH, 0);
            // A BGRA read is not in GLES's list without GL_EXT_read_format_bgra, but Mesa takes
            // it; if a driver refuses, the error below falls back to wgpu's own present.
            // wgpu's GL frames are stored top row first, which is X's row order too.
            gl.read_pixels(
                0,
                0,
                width as i32,
                height as i32,
                glow::BGRA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(pixels)),
            );
            let error = gl.get_error();
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
            if error != glow::NO_ERROR {
                return Err(format!("glReadPixels failed: {error:#x}"));
            }
        }
        drop(gl);
        shm::put_image(
            &self.conn,
            self.window,
            self.gc,
            w,
            h,
            0,
            0,
            w,
            h,
            0,
            0,
            self.depth,
            ImageFormat::Z_PIXMAP.into(),
            true,
            segment.id,
            0,
        )
        .map_err(|e| e.to_string())?;
        self.conn.flush().map_err(|e| e.to_string())?;
        self.busy = true;
        Ok(())
    }

    /// Waits until the server has copied the last frame out of the segment.
    fn wait_idle(&mut self) -> Result<(), String> {
        loop {
            let event = if self.busy {
                Some(self.conn.wait_for_event().map_err(|e| e.to_string())?)
            } else {
                self.conn.poll_for_event().map_err(|e| e.to_string())?
            };
            match event {
                Some(Event::ShmCompletion(_)) => self.busy = false,
                Some(Event::Error(error)) => return Err(format!("X error: {error:?}")),
                Some(_) => {}
                None => return Ok(()),
            }
        }
    }

    fn attach(&mut self, width: u32, height: u32) -> Result<(), String> {
        if let Some(old) = self.segment.take() {
            let _ = self.conn.shm_detach(old.id);
        }
        let len = width as usize * height as usize * 4;
        let fd = unsafe { libc::memfd_create(c"terminal-frame".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        if unsafe { libc::ftruncate(fd.as_raw_fd(), len as libc::off_t) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let segment = Segment {
            id: self.conn.generate_id().map_err(|e| e.to_string())?,
            ptr: ptr.cast(),
            len,
            size: (width, height),
        };
        self.conn
            .shm_attach_fd(segment.id, fd, true)
            .map_err(|e| e.to_string())?
            .check()
            .map_err(|e| e.to_string())?;
        self.segment = Some(segment);
        Ok(())
    }
}

/// Whether 32-bit little-endian pixels with these channel masks are BGRA bytes in memory.
fn bgra_layout(
    order: ImageOrder,
    bits_per_pixel: Option<u8>,
    masks: Option<(u32, u32, u32)>,
) -> bool {
    order == ImageOrder::LSB_FIRST
        && bits_per_pixel == Some(32)
        && masks == Some((0xff_0000, 0xff00, 0xff))
}

/// The frame size as `PutImage` dimensions, which are 16-bit.
fn put_size(width: u32, height: u32) -> Option<(u16, u16)> {
    Some((u16::try_from(width).ok()?, u16::try_from(height).ok()?)).filter(|&(w, h)| w > 0 && h > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_little_endian_32_bit_rgb_visuals_are_read_as_bgra() {
        let rgb = Some((0xff_0000, 0xff00, 0xff));
        assert!(bgra_layout(ImageOrder::LSB_FIRST, Some(32), rgb));
        assert!(!bgra_layout(ImageOrder::MSB_FIRST, Some(32), rgb));
        assert!(!bgra_layout(ImageOrder::LSB_FIRST, Some(24), rgb));
        assert!(!bgra_layout(
            ImageOrder::LSB_FIRST,
            Some(16),
            Some((0xf800, 0x7e0, 0x1f))
        ));
        assert!(!bgra_layout(
            ImageOrder::LSB_FIRST,
            Some(32),
            Some((0xff, 0xff00, 0xff_0000))
        ));
        assert!(!bgra_layout(ImageOrder::LSB_FIRST, None, rgb));
        assert!(!bgra_layout(ImageOrder::LSB_FIRST, Some(32), None));
    }

    #[test]
    fn put_size_rejects_empty_and_oversized_frames() {
        assert_eq!(put_size(1280, 720), Some((1280, 720)));
        assert_eq!(put_size(65535, 1), Some((65535, 1)));
        assert_eq!(put_size(65536, 1), None);
        assert_eq!(put_size(1, 70000), None);
        assert_eq!(put_size(0, 10), None);
    }
}
