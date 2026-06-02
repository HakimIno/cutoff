use std::borrow::Cow;
use wgpu::util::DeviceExt;

/// Input information for a single clip to be composited on the GPU.
pub struct GpuClipInput<'a> {
    pub rgba: &'a [u8],
    pub width: usize,
    pub height: usize,
    pub crop_l: usize,
    pub crop_t: usize,
    pub crop_r: usize,
    pub crop_b: usize,
    pub scale_x: f32,
    pub scale_y: f32,
    pub rotation_deg: f32,
    pub flip_h: bool,
    pub flip_v: bool,
    pub opacity: f32,
    pub center_x: f32,
    pub center_y: f32,
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
struct ClipUniforms {
    scale_x: f32,
    scale_y: f32,
    rotation_deg: f32,
    opacity: f32,
    center_x: f32,
    center_y: f32,
    base_w: f32,
    base_h: f32,
    src_w: f32,
    src_h: f32,
    crop_l: f32,
    crop_t: f32,
    crop_r: f32,
    crop_b: f32,
    flip_h: u32,
    flip_v: u32,
}

impl ClipUniforms {
    fn as_bytes(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                self as *const Self as *const u8,
                std::mem::size_of::<Self>(),
            )
        }
    }
}

pub struct WgpuCompositor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    target_format: wgpu::TextureFormat,
    target_texture: Option<wgpu::Texture>,
    target_view: Option<wgpu::TextureView>,
    readback_buffer: Option<wgpu::Buffer>,
    current_w: usize,
    current_h: usize,
    bind_group_layout: wgpu::BindGroupLayout,
}

impl WgpuCompositor {
    /// Try to initialize the GPU compositor. Returns Err if wgpu is not supported
    /// or if no compatible GPU adapter is found.
    pub fn new() -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let adapter = futures::executor::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok_or_else(|| "Failed to find a compatible wgpu adapter. GPU composting unavailable.".to_string())?;

        let (device, queue) = futures::executor::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("Compositor Device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
            },
            None,
        ))
        .map_err(|e| format!("Failed to request wgpu device: {}", e))?;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Composite Shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("composite.wgsl"))),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Clip Bind Group Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Compositor Pipeline Layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let target_format = wgpu::TextureFormat::Rgba8Unorm;

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Compositor Render Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::SrcAlpha,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Compositor Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        Ok(Self {
            device,
            queue,
            pipeline,
            sampler,
            target_format,
            target_texture: None,
            target_view: None,
            readback_buffer: None,
            current_w: 0,
            current_h: 0,
            bind_group_layout,
        })
    }

    /// Blend all active clips on the GPU and return the composited pixels in `base`.
    pub fn composite(
        &mut self,
        base_w: usize,
        base_h: usize,
        base: &mut [u8],
        clips: &[GpuClipInput<'_>],
    ) -> Result<(), String> {
        if base_w == 0 || base_h == 0 {
            return Ok(());
        }

        // 1. Re-allocate target texture and readback buffer if size changed
        if self.target_texture.is_none() || self.current_w != base_w || self.current_h != base_h {
            let texture_desc = wgpu::TextureDescriptor {
                label: Some("Compositor Target Texture"),
                size: wgpu::Extent3d {
                    width: base_w as u32,
                    height: base_h as u32,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.target_format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            };
            let texture = self.device.create_texture(&texture_desc);
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

            // Readback buffer must have padded row size (wgpu requires row bytes to be multiple of 256)
            let raw_row_bytes = base_w * 4;
            let align = 256;
            let padded_row_bytes = (raw_row_bytes + align - 1) & !(align - 1);
            let buffer_size = (padded_row_bytes * base_h) as u64;

            let readback_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Compositor Readback Buffer"),
                size: buffer_size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });

            self.target_texture = Some(texture);
            self.target_view = Some(view);
            self.readback_buffer = Some(readback_buffer);
            self.current_w = base_w;
            self.current_h = base_h;
        }

        let target_view = self.target_view.as_ref().unwrap();

        // 2. Initialize command encoder
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Compositor Command Encoder"),
        });

        // 3. Clear target to transparent black
        {
            let _render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Clear Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 0.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }

        // 4. Draw each clip in order
        for clip in clips {
            if clip.width == 0 || clip.height == 0 || clip.opacity <= 0.0 {
                continue;
            }

            // Create texture for the clip
            let clip_texture_desc = wgpu::TextureDescriptor {
                label: Some("Clip Texture"),
                size: wgpu::Extent3d {
                    width: clip.width as u32,
                    height: clip.height as u32,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            };
            let clip_texture = self.device.create_texture(&clip_texture_desc);
            let clip_view = clip_texture.create_view(&wgpu::TextureViewDescriptor::default());

            // Upload RGBA data
            self.queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &clip_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                clip.rgba,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(clip.width as u32 * 4),
                    rows_per_image: Some(clip.height as u32),
                },
                wgpu::Extent3d {
                    width: clip.width as u32,
                    height: clip.height as u32,
                    depth_or_array_layers: 1,
                },
            );

            // Create uniforms buffer
            let uniforms = ClipUniforms {
                scale_x: clip.scale_x,
                scale_y: clip.scale_y,
                rotation_deg: clip.rotation_deg,
                opacity: clip.opacity,
                center_x: clip.center_x,
                center_y: clip.center_y,
                base_w: base_w as f32,
                base_h: base_h as f32,
                src_w: clip.width as f32,
                src_h: clip.height as f32,
                crop_l: clip.crop_l as f32,
                crop_t: clip.crop_t as f32,
                crop_r: clip.crop_r as f32,
                crop_b: clip.crop_b as f32,
                flip_h: if clip.flip_h { 1 } else { 0 },
                flip_v: if clip.flip_v { 1 } else { 0 },
            };

            let uniforms_buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Uniforms Buffer"),
                contents: uniforms.as_bytes(),
                usage: wgpu::BufferUsages::UNIFORM,
            });

            // Create BindGroup
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Clip Bind Group"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniforms_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&clip_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });

            // Draw Pass
            {
                let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Draw Clip Pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                render_pass.set_pipeline(&self.pipeline);
                render_pass.set_bind_group(0, &bind_group, &[]);
                render_pass.draw(0..4, 0..1);
            }
        }

        // 5. Copy target texture to readback buffer
        let raw_row_bytes = base_w * 4;
        let align = 256;
        let padded_row_bytes = (raw_row_bytes + align - 1) & !(align - 1);

        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: self.target_texture.as_ref().unwrap(),
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: self.readback_buffer.as_ref().unwrap(),
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row_bytes as u32),
                    rows_per_image: Some(base_h as u32),
                },
            },
            wgpu::Extent3d {
                width: base_w as u32,
                height: base_h as u32,
                depth_or_array_layers: 1,
            },
        );

        // 6. Submit queue
        self.queue.submit(Some(encoder.finish()));

        // 7. Map readback buffer and copy to `base`
        let buffer = self.readback_buffer.as_ref().unwrap();
        let buffer_slice = buffer.slice(..);

        let (tx, rx) = std::sync::mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |v| {
            let _ = tx.send(v);
        });

        self.device.poll(wgpu::Maintain::Wait);

        rx.recv()
            .map_err(|e| format!("Map channel recv error: {}", e))?
            .map_err(|e| format!("Buffer mapping failed: {}", e))?;

        {
            let view = buffer_slice.get_mapped_range();
            // Copy row-by-row to discard padding bytes
            for y in 0..base_h {
                let src_offset = y * padded_row_bytes;
                let dest_offset = y * base_w * 4;
                base[dest_offset..dest_offset + raw_row_bytes]
                    .copy_from_slice(&view[src_offset..src_offset + raw_row_bytes]);
            }
        }

        buffer.unmap();

        Ok(())
    }
}
