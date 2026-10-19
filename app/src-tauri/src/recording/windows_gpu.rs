//! Owner-thread D3D11 conversion of cropped WGC frames to limited-range BT.709 NV12.
use std::{marker::PhantomData, rc::Rc};
use windows::{
    Win32::Graphics::{
        Direct3D::{D3D_SRV_DIMENSION_BUFFER, Fxc::D3DCompile, ID3DBlob},
        Direct3D11::*,
        Dxgi::Common::*,
    },
    core::{Interface, s},
};

const TRANSFER_ENTRIES: u32 = 4097 + 256;

#[derive(Clone, Copy, Debug)]
struct Layout {
    width: u32,
    height: u32,
    pitch: u32,
    buffer_bytes: u32,
    packed_bytes: usize,
}
impl Layout {
    fn new(width: u32, height: u32) -> Result<Self, String> {
        // cs_5_0 and the source texture require D3D feature level 11.
        if width == 0 || height == 0 || width > 16_384 || height > 16_384 {
            return Err(
                "GPU conversion requires a nonempty region at most 16384 pixels per edge".into(),
            );
        }
        let width = (width + 1) & !1;
        let height = (height + 1) & !1;
        let pitch = (width + 3) & !3;
        let buffer_bytes = pitch * height * 3 / 2;
        Ok(Self {
            width,
            height,
            pitch,
            buffer_bytes,
            packed_bytes: (width * height * 3 / 2) as usize,
        })
    }
}

/// Owns one cropped RGB texture, one GPU NV12 buffer, one readback NV12 buffer,
/// a 17,412-byte immutable transfer table, and a 32-byte constant buffer.
///
/// RGB payload is selected_width * selected_height * (HDR: 8, SDR: 4).
/// Each NV12 buffer is align_up(even_width, 4) * even_height * 3 / 2 bytes.
/// A conversion allocates one compact even_width * even_height * 3 / 2 CPU Vec.
/// Driver alignment, WGC textures and caller-retained output are additional.
/// The immediate context belongs exclusively to the calling capture thread;
/// this type deliberately cannot be sent or shared between threads.
pub struct Converter {
    context: ID3D11DeviceContext,
    texture: ID3D11Texture2D,
    source_view: ID3D11ShaderResourceView,
    transfer_view: ID3D11ShaderResourceView,
    constants: ID3D11Buffer,
    shader: ID3D11ComputeShader,
    output: ID3D11Buffer,
    output_view: ID3D11UnorderedAccessView,
    readback: ID3D11Buffer,
    selected_width: u32,
    selected_height: u32,
    format: DXGI_FORMAT,
    layout: Layout,
    _owner_thread: PhantomData<Rc<()>>,
}

impl Converter {
    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        width: u32,
        height: u32,
        hdr: bool,
        white: f32,
    ) -> Result<Self, String> {
        let layout = Layout::new(width, height)?;
        if !white.is_finite() || white <= 0.0 {
            return Err("GPU conversion requires a finite positive SDR white level".into());
        }
        unsafe {
            if device.GetFeatureLevel().0
                < windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0.0
            {
                return Err("GPU recording conversion requires Direct3D feature level 11".into());
            }
            let owner = context.GetDevice().map_err(gpu_error)?;
            if owner.as_raw() != device.as_raw() {
                return Err("GPU converter device and context do not match".into());
            }
            let format = if hdr {
                DXGI_FORMAT_R16G16B16A16_FLOAT
            } else {
                DXGI_FORMAT_B8G8R8A8_UNORM
            };
            let mut texture = None;
            device
                .CreateTexture2D(
                    &D3D11_TEXTURE2D_DESC {
                        Width: width,
                        Height: height,
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: format,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        Usage: D3D11_USAGE_DEFAULT,
                        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                        ..Default::default()
                    },
                    None,
                    Some(&mut texture),
                )
                .map_err(gpu_error)?;
            let texture = required(texture, "crop texture")?;
            let mut source_view = None;
            device
                .CreateShaderResourceView(&texture, None, Some(&mut source_view))
                .map_err(gpu_error)?;

            let transfer = transfer_table();
            let transfer_buffer = create_buffer(
                device,
                &D3D11_BUFFER_DESC {
                    ByteWidth: TRANSFER_ENTRIES * 4,
                    Usage: D3D11_USAGE_IMMUTABLE,
                    BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                    ..Default::default()
                },
                Some(transfer.as_ptr().cast()),
            )?;
            let mut transfer_view = None;
            device
                .CreateShaderResourceView(
                    &transfer_buffer,
                    Some(&D3D11_SHADER_RESOURCE_VIEW_DESC {
                        Format: DXGI_FORMAT_R32_UINT,
                        ViewDimension: D3D_SRV_DIMENSION_BUFFER,
                        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                            Buffer: D3D11_BUFFER_SRV {
                                Anonymous1: D3D11_BUFFER_SRV_0 { FirstElement: 0 },
                                Anonymous2: D3D11_BUFFER_SRV_1 {
                                    NumElements: TRANSFER_ENTRIES,
                                },
                            },
                        },
                    }),
                    Some(&mut transfer_view),
                )
                .map_err(gpu_error)?;
            // Two HLSL registers. No per-frame constant updates or map allocation.
            let constants_data = [
                width,
                height,
                layout.width,
                layout.height,
                layout.pitch,
                u32::from(hdr),
                white.to_bits(),
                0,
            ];
            let constants = create_buffer(
                device,
                &D3D11_BUFFER_DESC {
                    ByteWidth: 32,
                    Usage: D3D11_USAGE_IMMUTABLE,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    ..Default::default()
                },
                Some(constants_data.as_ptr().cast()),
            )?;
            let output = create_buffer(
                device,
                &D3D11_BUFFER_DESC {
                    ByteWidth: layout.buffer_bytes,
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_UNORDERED_ACCESS.0 as u32,
                    MiscFlags: D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS.0 as u32,
                    ..Default::default()
                },
                None,
            )?;
            let mut output_view = None;
            device
                .CreateUnorderedAccessView(
                    &output,
                    Some(&D3D11_UNORDERED_ACCESS_VIEW_DESC {
                        Format: DXGI_FORMAT_R32_TYPELESS,
                        ViewDimension: D3D11_UAV_DIMENSION_BUFFER,
                        Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC_0 {
                            Buffer: D3D11_BUFFER_UAV {
                                FirstElement: 0,
                                NumElements: layout.buffer_bytes / 4,
                                Flags: D3D11_BUFFER_UAV_FLAG_RAW.0 as u32,
                            },
                        },
                    }),
                    Some(&mut output_view),
                )
                .map_err(gpu_error)?;
            let readback = create_buffer(
                device,
                &D3D11_BUFFER_DESC {
                    ByteWidth: layout.buffer_bytes,
                    Usage: D3D11_USAGE_STAGING,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    ..Default::default()
                },
                None,
            )?;
            let bytecode = compile_shader()?;
            let bytes = std::slice::from_raw_parts(
                bytecode.GetBufferPointer().cast(),
                bytecode.GetBufferSize(),
            );
            let mut shader = None;
            device
                .CreateComputeShader(bytes, None, Some(&mut shader))
                .map_err(gpu_error)?;
            Ok(Self {
                context: context.clone(),
                texture,
                source_view: required(source_view, "source view")?,
                transfer_view: required(transfer_view, "transfer view")?,
                constants,
                shader: required(shader, "compute shader")?,
                output,
                output_view: required(output_view, "output view")?,
                readback,
                selected_width: width,
                selected_height: height,
                format,
                layout,
                _owner_thread: PhantomData,
            })
        }
    }

    /// Copies and converts one selected source region. Clears its compute slots
    /// before readback; the caller must not share these slots during conversion.
    pub fn convert(&self, texture: &ID3D11Texture2D, crop: &D3D11_BOX) -> Result<Vec<u8>, String> {
        unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            texture.GetDesc(&mut desc);
            if desc.Format != self.format
                || desc.SampleDesc.Count != 1
                || crop.front != 0
                || crop.back != 1
                || crop.left.checked_add(self.selected_width) != Some(crop.right)
                || crop.top.checked_add(self.selected_height) != Some(crop.bottom)
                || crop.right > desc.Width
                || crop.bottom > desc.Height
            {
                return Err("GPU conversion source format or selected bounds changed".into());
            }
            let owner = texture.GetDevice().map_err(gpu_error)?;
            let expected_owner = self.context.GetDevice().map_err(gpu_error)?;
            if owner.as_raw() != expected_owner.as_raw() {
                return Err("GPU conversion source belongs to another device".into());
            }
            self.context
                .CopySubresourceRegion(&self.texture, 0, 0, 0, 0, texture, 0, Some(crop));
            self.context.CSSetShader(&self.shader, None);
            self.context.CSSetShaderResources(
                0,
                Some(&[
                    Some(self.source_view.clone()),
                    Some(self.transfer_view.clone()),
                ]),
            );
            self.context
                .CSSetConstantBuffers(0, Some(&[Some(self.constants.clone())]));
            self.context.CSSetUnorderedAccessViews(
                0,
                1,
                Some([Some(self.output_view.clone())].as_ptr()),
                None,
            );
            self.context.Dispatch(
                (self.layout.pitch / 4).div_ceil(8),
                (self.layout.height / 2).div_ceil(8),
                1,
            );
            self.context
                .CSSetUnorderedAccessViews(0, 1, Some([None].as_ptr()), None);
            self.context.CSSetShaderResources(0, Some(&[None, None]));
            self.context.CSSetConstantBuffers(0, Some(&[None]));
            self.context.CSSetShader(None, None);
            self.context.CopyResource(&self.readback, &self.output);
            // Allocate before Map, so allocation failure cannot leave a mapped resource.
            let mut compact = Vec::new();
            compact
                .try_reserve_exact(self.layout.packed_bytes)
                .map_err(|e| format!("NV12 readback allocation: {e}"))?;
            compact.resize(self.layout.packed_bytes, 0);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&self.readback, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(gpu_error)?;
            // Buffers have linear ByteWidth storage; texture RowPitch does not apply.
            let source = std::slice::from_raw_parts(
                mapped.pData.cast::<u8>(),
                self.layout.buffer_bytes as usize,
            );
            for (row, dst) in compact
                .chunks_exact_mut(self.layout.width as usize)
                .enumerate()
            {
                let offset = row * self.layout.pitch as usize;
                dst.copy_from_slice(&source[offset..offset + dst.len()]);
            }
            self.context.Unmap(&self.readback, 0);
            Ok(compact)
        }
    }
}

fn gpu_error(error: windows::core::Error) -> String {
    format!("GPU recording conversion: {error}")
}
fn required<T>(value: Option<T>, label: &str) -> Result<T, String> {
    value.ok_or_else(|| format!("GPU recording conversion returned no {label}"))
}
fn create_buffer(
    device: &ID3D11Device,
    desc: &D3D11_BUFFER_DESC,
    data: Option<*const std::ffi::c_void>,
) -> Result<ID3D11Buffer, String> {
    let data = data.map(|p_sys_mem| D3D11_SUBRESOURCE_DATA {
        pSysMem: p_sys_mem,
        ..Default::default()
    });
    let mut buffer = None;
    unsafe {
        device.CreateBuffer(
            desc,
            data.as_ref().map(|d| d as *const _),
            Some(&mut buffer),
        )
    }
    .map_err(gpu_error)?;
    required(buffer, "buffer")
}
fn bt709_transfer(linear: f32) -> u32 {
    let value = if linear < 0.018 {
        4.5 * linear
    } else {
        1.099 * linear.powf(0.45) - 0.099
    };
    (value * 255.0).round().clamp(0.0, 255.0) as u32
}
fn transfer_table() -> Vec<u32> {
    (0..4097)
        .map(|i| bt709_transfer(i as f32 / 4096.0))
        .chain((0..256).map(|i| {
            let encoded = i as f32 / 255.0;
            bt709_transfer(if encoded <= 0.04045 {
                encoded / 12.92
            } else {
                ((encoded + 0.055) / 1.055).powf(2.4)
            })
        }))
        .collect()
}
fn compile_shader() -> Result<ID3DBlob, String> {
    let (mut code, mut messages) = (None, None);
    let result = unsafe {
        D3DCompile(
            SHADER.as_ptr().cast(),
            SHADER.len(),
            s!("recording-nv12"),
            None,
            None,
            s!("main"),
            s!("cs_5_0"),
            1 << 11,
            0,
            &mut code,
            Some(&mut messages),
        )
    };
    if let Err(error) = result {
        let detail = messages
            .as_ref()
            .map(|blob| unsafe {
                String::from_utf8_lossy(std::slice::from_raw_parts(
                    blob.GetBufferPointer().cast(),
                    blob.GetBufferSize(),
                ))
                .trim_end_matches('\0')
                .to_owned()
            })
            .unwrap_or_default();
        return Err(format!(
            "GPU recording shader compilation: {error}: {detail}"
        ));
    }
    required(code, "compiled shader")
}

const SHADER: &str = r#"
Texture2D<float4> Source : register(t0);
Buffer<uint> Transfer : register(t1);
RWByteAddressBuffer Output : register(u0);
cbuffer Parameters : register(b0) {
    uint SelectedWidth, SelectedHeight, Width, Height;
    uint Pitch, Hdr; float White; uint Reserved;
};
uint3 pixel(uint x, uint y) {
    if (x >= SelectedWidth || y >= SelectedHeight) return uint3(0, 0, 0);
    float3 rgb = Source.Load(int3(x, y, 0)).rgb;
    if (Hdr == 0) {
        uint3 v = (uint3)floor(saturate(rgb) * 255.0 + 0.5);
        return uint3(Transfer[4097 + v.r], Transfer[4097 + v.g], Transfer[4097 + v.b]);
    }
    rgb = float3(isfinite(rgb.r) ? rgb.r / White : 0.0,
                  isfinite(rgb.g) ? rgb.g / White : 0.0,
                  isfinite(rgb.b) ? rgb.b / White : 0.0);
    precise float luma = max(rgb.r * 0.2126 + rgb.g * 0.7152 + rgb.b * 0.0722, 0.0);
    float mapped = luma;
    if (luma > 0.75) { float highlight = luma - 0.75; mapped = 0.75 + 0.25 * highlight / (highlight + 0.25); }
    if (luma > 0.0) rgb *= mapped / luma;
    float chroma = 1.0;
    [unroll] for (uint c = 0; c < 3; ++c) {
        if (rgb[c] > 1.0) chroma = min(chroma, (1.0 - mapped) / (rgb[c] - mapped));
        if (rgb[c] < 0.0) chroma = min(chroma, mapped / (mapped - rgb[c]));
    }
    uint3 index = (uint3)floor(saturate(mapped + (rgb - mapped) * chroma) * 4096.0 + 0.5);
    return uint3(Transfer[index.r], Transfer[index.g], Transfer[index.b]);
}
uint y709(uint3 p) { return (uint)clamp(((47 * (int)p.r + 157 * (int)p.g + 16 * (int)p.b + 128) >> 8) + 16, 16, 235); }
uint uv709(uint3 a, uint3 b, uint3 c, uint3 d) {
    int3 p = (int3)((a + b + c + d) / 4);
    uint u = (uint)clamp(((-26 * p.r - 86 * p.g + 112 * p.b + 128) >> 8) + 128, 16, 240);
    uint v = (uint)clamp(((112 * p.r - 102 * p.g - 10 * p.b + 128) >> 8) + 128, 16, 240);
    return u | (v << 8);
}
uint pack4(uint a, uint b, uint c, uint d) { return a | (b << 8) | (c << 16) | (d << 24); }
[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    uint x = id.x * 4, y = id.y * 2;
    if (x >= Pitch || y >= Height) return;
    uint3 a = pixel(x, y), b = pixel(x + 1, y), c = pixel(x + 2, y), d = pixel(x + 3, y);
    uint3 e = pixel(x, y + 1), f = pixel(x + 1, y + 1), g = pixel(x + 2, y + 1), h = pixel(x + 3, y + 1);
    Output.Store(y * Pitch + x, pack4(y709(a), y709(b), y709(c), y709(d)));
    Output.Store((y + 1) * Pitch + x, pack4(y709(e), y709(f), y709(g), y709(h)));
    Output.Store(Height * Pitch + (y / 2) * Pitch + x, uv709(a, b, e, f) | (uv709(c, d, g, h) << 16));
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nv12_layout_preserves_odd_region_and_word_alignment() {
        let layout = Layout::new(641, 481).unwrap();
        assert_eq!((layout.width, layout.height, layout.pitch), (642, 482, 644));
        assert_eq!(layout.buffer_bytes, 465_612);
        assert_eq!(layout.packed_bytes, 464_166);
        let full = Layout::new(7680, 2160).unwrap();
        assert_eq!(full.buffer_bytes, 24_883_200);
        assert_eq!(Layout::new(1, 1).unwrap().buffer_bytes, 12);
        assert!(Layout::new(0, 2).is_err());
        assert!(Layout::new(u32::MAX, 2).is_err());
    }
    #[test]
    fn transfer_lut_has_distinct_hdr_and_srgb_curves() {
        let lut = transfer_table();
        assert_eq!(lut.len(), TRANSFER_ENTRIES as usize);
        assert_eq!((lut[0], lut[4096], lut[4097], lut[4352]), (0, 255, 0, 255));
        assert!(lut[..4097].windows(2).all(|v| v[0] <= v[1]));
        assert!(lut[4097..].windows(2).all(|v| v[0] <= v[1]));
        assert_eq!(lut[4097 + 128], 115);
    }
}
