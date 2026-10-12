//! Window surfaces: a wgpu surface plus the device that presents to it. Any window type that
//! provides raw-window-handle 0.6 handles works (winit on Linux, an NSView/CAMetalLayer shell).

use wgpu::{CurrentSurfaceTexture, PresentMode, SurfaceTexture, TextureFormat};

/// A configured window surface and its GPU.
pub struct SurfaceGpu {
    pub instance: wgpu::Instance,
    pub surface: wgpu::Surface<'static>,
    pub adapter_info: wgpu::AdapterInfo,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub config: wgpu::SurfaceConfiguration,
}

/// The renderer blends in gamma space (like Console), so an sRGB swapchain would wash colours
/// out: prefer 8-bit non-sRGB formats.
pub fn pick_format(formats: &[TextureFormat]) -> Option<TextureFormat> {
    [TextureFormat::Bgra8Unorm, TextureFormat::Rgba8Unorm]
        .into_iter()
        .find(|f| formats.contains(f))
        .or_else(|| formats.iter().copied().find(|f| !f.is_srgb()))
        .or_else(|| formats.first().map(TextureFormat::remove_srgb_suffix))
}

/// Fifo (vsync, always supported per spec), falling back to Mailbox.
pub fn pick_present_mode(modes: &[PresentMode]) -> PresentMode {
    [PresentMode::Fifo, PresentMode::Mailbox]
        .into_iter()
        .find(|m| modes.contains(m))
        .unwrap_or(PresentMode::AutoVsync)
}

impl SurfaceGpu {
    /// `display` is the platform display connection (winit's `OwnedDisplayHandle`), `target`
    /// the window (e.g. `Arc<winit::window::Window>`), `size` its inner size in pixels.
    pub fn new(
        display: Box<dyn wgpu::wgt::WgpuHasDisplayHandle>,
        target: impl Into<wgpu::SurfaceTarget<'static>>,
        size: (u32, u32),
    ) -> Result<Self, String> {
        let instance = wgpu::Instance::new(
            wgpu::InstanceDescriptor::new_with_display_handle_from_env(display),
        );
        let surface = instance
            .create_surface(target)
            .map_err(|e| format!("create surface: {e}"))?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .map_err(|e| format!("no GPU adapter for the window: {e}"))?;
        let adapter_info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("uaconsole"),
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .map_err(|e| format!("request device: {e}"))?;
        let caps = surface.get_capabilities(&adapter);
        let (w, h) = (size.0.max(1), size.1.max(1));
        let mut config = surface
            .get_default_config(&adapter, w, h)
            .ok_or("surface is not supported by the adapter")?;
        config.format = pick_format(&caps.formats).ok_or("surface reports no formats")?;
        config.present_mode = pick_present_mode(&caps.present_modes);
        config.desired_maximum_frame_latency = 2;
        surface.configure(&device, &config);
        Ok(Self {
            instance,
            surface,
            adapter_info,
            device,
            queue,
            config,
        })
    }

    pub fn format(&self) -> TextureFormat {
        self.config.format
    }

    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Reconfigures for a new inner size (ignored while zero, e.g. minimised).
    pub fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 || (w, h) == self.size() {
            return;
        }
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.device, &self.config);
    }

    /// Next frame to draw into, or None to skip this frame (occluded, timed out, or the
    /// swapchain had to be rebuilt). Present with [`SurfaceGpu::present`].
    pub fn acquire(&mut self) -> Option<SurfaceTexture> {
        match self.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(t) => Some(t),
            CurrentSurfaceTexture::Suboptimal(t) => {
                // Draw this one; reconfigure for the next.
                self.surface.configure(&self.device, &self.config);
                Some(t)
            }
            CurrentSurfaceTexture::Outdated | CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                match self.surface.get_current_texture() {
                    CurrentSurfaceTexture::Success(t) | CurrentSurfaceTexture::Suboptimal(t) => {
                        Some(t)
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    pub fn present(&self, frame: SurfaceTexture) {
        self.queue.present(frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_non_srgb() {
        let f = [
            TextureFormat::Bgra8UnormSrgb,
            TextureFormat::Bgra8Unorm,
            TextureFormat::Rgba16Float,
        ];
        assert_eq!(pick_format(&f), Some(TextureFormat::Bgra8Unorm));
        assert_eq!(
            pick_format(&[TextureFormat::Rgba8UnormSrgb, TextureFormat::Rgba16Float]),
            Some(TextureFormat::Rgba16Float)
        );
        assert_eq!(
            pick_format(&[TextureFormat::Rgba8UnormSrgb]),
            Some(TextureFormat::Rgba8Unorm)
        );
        assert_eq!(pick_format(&[]), None);
    }

    #[test]
    fn present_mode_fallback() {
        assert_eq!(
            pick_present_mode(&[PresentMode::Mailbox, PresentMode::Fifo]),
            PresentMode::Fifo
        );
        assert_eq!(
            pick_present_mode(&[PresentMode::Immediate, PresentMode::Mailbox]),
            PresentMode::Mailbox
        );
    }
}
