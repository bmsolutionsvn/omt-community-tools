//! Captured frames kept in video memory: a GPU copy into textures this app owns, nothing read back.

use std::ffi::c_void;
use std::sync::Arc;

use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_SUBRESOURCE_DATA,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Device, ID3D11DeviceContext,
    ID3D11Multithread, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::core::Interface;

/// A BGRA texture on the capture device.
pub struct GpuTexture(ID3D11Texture2D);

// The capture device's immediate context is multithread-protected (`GpuRing::new`), so the sender
// thread may hand this texture to omthx while the capture thread copies into another one.
unsafe impl Send for GpuTexture {}
unsafe impl Sync for GpuTexture {}

impl GpuTexture {
    /// The `ID3D11Texture2D` pointer, valid while `self` lives.
    pub fn as_raw(&self) -> *mut c_void {
        self.0.as_raw()
    }
}

/// Textures the capture thread copies each frame into; one is reused once nothing else holds it.
pub struct GpuRing {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    size: (u32, u32),
    textures: Vec<Arc<GpuTexture>>,
}

// Used only on the capture thread; the device and context are free-threaded once protected.
unsafe impl Send for GpuRing {}

impl GpuRing {
    /// Turn on the context's own locking: this thread copies while the sender thread converts and encodes.
    pub fn new(device: ID3D11Device, context: ID3D11DeviceContext) -> windows::core::Result<Self> {
        let _ = unsafe {
            context
                .cast::<ID3D11Multithread>()?
                .SetMultithreadProtected(true)
        };
        Ok(Self {
            device,
            context,
            size: (0, 0),
            textures: Vec::new(),
        })
    }

    /// Copy `source`'s top-left `width` x `height` into a free texture of the even-rounded size, whose
    /// extra column and row stay black (OMT-HX rejects odd sizes).
    pub fn copy(
        &mut self,
        source: &ID3D11Texture2D,
        width: u32,
        height: u32,
    ) -> windows::core::Result<Arc<GpuTexture>> {
        let size = (width + width % 2, height + height % 2);
        if size != self.size {
            self.textures.clear(); // a texture still being sent stays alive through its own Arc
            self.size = size;
        }
        // Held only here: not waiting in the sink, not the sender's last frame, not being sent.
        let free = self
            .textures
            .iter()
            .find(|t| Arc::strong_count(t) == 1)
            .cloned();
        let texture = match free {
            Some(texture) => texture,
            None => {
                let texture = Arc::new(GpuTexture(self.create(size)?));
                self.textures.push(Arc::clone(&texture));
                texture
            }
        };
        let region = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: width,
            bottom: height,
            back: 1,
        };
        unsafe {
            self.context
                .CopySubresourceRegion(&texture.0, 0, 0, 0, 0, source, 0, Some(&region));
            // Source goes back to Windows' frame pool when this returns, so submit the read now.
            self.context.Flush();
        }
        Ok(texture)
    }

    fn create(&self, (width, height): (u32, u32)) -> windows::core::Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            // omthx's video processor reads it.
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        // Black to start with, so the padding of an odd-sized window is black, as on the memory path.
        let black = vec![0u8; width as usize * height as usize * 4];
        let init = D3D11_SUBRESOURCE_DATA {
            pSysMem: black.as_ptr().cast(),
            SysMemPitch: width * 4,
            SysMemSlicePitch: 0,
        };
        let mut texture = None;
        unsafe {
            self.device
                .CreateTexture2D(&desc, Some(&init), Some(&mut texture))?;
        }
        texture.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_POINTER))
    }
}
