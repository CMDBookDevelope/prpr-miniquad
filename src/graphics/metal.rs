//! Native Metal resources using the fork's existing Texture/Buffer handles.
//! An offscreen context never creates UIKit/AppKit windows; Tauri owns the UI.
use super::*;
use metal_rs as m;
use std::{collections::HashMap, ffi::c_void};

pub struct MetalPass {
    pub color: Texture,
    pub samples: u32,
    msaa: Option<m::Texture>,
    depth: Option<m::Texture>,
}
struct MetalTexture {
    image: m::Texture,
    sampler: m::SamplerState,
    params: TextureParams,
}
struct MetalShader {
    vertex: m::Function,
    fragment: m::Function,
    layout: UniformBlockLayout,
}
pub struct MetalPipeline {
    pub params: PipelineParams,
    shader: usize,
    vertices: m::VertexDescriptor,
    variants: HashMap<(u64, bool), m::RenderPipelineState>,
}

pub struct MetalContext {
    pub device: m::Device,
    queue: m::CommandQueue,
    command: Option<m::CommandBuffer>,
    encoder: Option<m::RenderCommandEncoder>,
    textures: Vec<Option<MetalTexture>>,
    buffers: Vec<Option<m::Buffer>>,
    shaders: Vec<MetalShader>,
    pub pipelines: Vec<MetalPipeline>,
    pub passes: Vec<Option<MetalPass>>,
    default_color: Texture,
    current_pass: Option<usize>,
    pipeline: Option<usize>,
    index: Option<Buffer>,
    // Retain each buffer snapshot until the GPU finishes. A streaming buffer
    // may be updated multiple times in a frame; previous draws need old data.
    transient: Vec<m::Buffer>,
    pub max_texture_size: u32,
    scale_pipeline: Option<m::RenderPipelineState>,
}

impl MetalContext {
    pub fn gpu_info() -> Option<(String, Vec<u32>)> {
        let device = m::Device::system_default()?;
        let counts = [1, 2, 4, 8, 16, 32, 64]
            .iter()
            .copied()
            .filter(|&count| device.supports_texture_sample_count(count as u64))
            .collect();
        Some((device.name().to_owned(), counts))
    }
    pub fn new(size: (u32, u32)) -> Self {
        let device = m::Device::system_default().expect("Metal GPU is unavailable");
        let queue = device.new_command_queue();
        let mut context = Self {
            device,
            queue,
            command: None,
            encoder: None,
            textures: Vec::new(),
            buffers: Vec::new(),
            shaders: Vec::new(),
            pipelines: Vec::new(),
            passes: Vec::new(),
            default_color: Texture::empty(),
            current_pass: None,
            pipeline: None,
            index: None,
            transient: Vec::new(),
            max_texture_size: 16384,
            scale_pipeline: None,
        };
        // Macroquad binds Texture::empty() for unused sampler slots. Keep a
        // valid white fallback at handle zero, as the other backends do.
        context.new_texture(
            Some(&[255, 255, 255, 255]),
            TextureParams {
                width: 1,
                height: 1,
                ..Default::default()
            },
            TextureAccess::Static,
        );
        let params = TextureParams {
            width: size.0,
            height: size.1,
            ..Default::default()
        };
        let index = context.new_texture(None, params, TextureAccess::RenderTarget);
        context.default_color = Texture {
            texture: index as u32,
            width: size.0,
            height: size.1,
            format: params.format,
            wrap: params.wrap,
            filter: params.filter,
            mipmap_filter: params.mipmap_filter,
        };
        context
    }
    fn image(&self, index: usize) -> &m::TextureRef {
        &self.textures[index]
            .as_ref()
            .expect("deleted Metal texture")
            .image
    }
    fn command(&mut self) -> &m::CommandBufferRef {
        if self.command.is_none() {
            self.command = Some(self.queue.new_command_buffer().to_owned());
        }
        self.command.as_ref().unwrap()
    }
    pub fn finish(&mut self) {
        self.end_render_pass();
        if let Some(command) = self.command.take() {
            command.commit();
            command.wait_until_completed();
            assert!(
                command.status() != m::MTLCommandBufferStatus::Error,
                "Metal command buffer failed"
            );
        }
        self.transient.clear();
    }
    pub fn new_shader(
        &mut self,
        vertex: &str,
        fragment: &str,
        meta: ShaderMeta,
    ) -> Result<Shader, ShaderError> {
        let compile = |source: &str, kind, stage| -> Result<m::Function, ShaderError> {
            let source = super::metal_shader::translate(source, kind).map_err(|error_message| {
                ShaderError::CompilationError {
                    shader_type: stage,
                    error_message,
                }
            })?;
            self.device
                .new_library_with_source(&source, &m::CompileOptions::new())
                .and_then(|library| library.get_function("main0", None))
                .map_err(ShaderError::LinkError)
        };
        self.shaders.push(MetalShader {
            vertex: compile(vertex, shaderc::ShaderKind::Vertex, ShaderType::Vertex)?,
            fragment: compile(
                fragment,
                shaderc::ShaderKind::Fragment,
                ShaderType::Fragment,
            )?,
            layout: meta.uniforms,
        });
        Ok(Shader(self.shaders.len() - 1))
    }
    pub fn new_pipeline(
        &mut self,
        layouts: &[BufferLayout],
        attrs: &[VertexAttribute],
        shader: Shader,
        params: PipelineParams,
    ) -> Pipeline {
        let vertices = m::VertexDescriptor::new();
        let mut offsets = vec![0u64; layouts.len()];
        for (location, attribute) in attrs.iter().enumerate() {
            let desc = vertices.attributes().object_at(location as u64).unwrap();
            desc.set_format(vertex_format(attribute.format));
            desc.set_buffer_index(attribute.buffer_index as u64 + 1);
            desc.set_offset(offsets[attribute.buffer_index]);
            offsets[attribute.buffer_index] += attribute.format.byte_len() as u64;
        }
        for (index, layout) in layouts.iter().enumerate() {
            let desc = vertices.layouts().object_at(index as u64 + 1).unwrap();
            desc.set_stride(if layout.stride == 0 {
                offsets[index]
            } else {
                layout.stride as u64
            });
            desc.set_step_function(match layout.step_func {
                VertexStep::PerVertex => m::MTLVertexStepFunction::PerVertex,
                VertexStep::PerInstance => m::MTLVertexStepFunction::PerInstance,
            });
            desc.set_step_rate(layout.step_rate as u64);
        }
        self.pipelines.push(MetalPipeline {
            shader: shader.0,
            params,
            vertices: vertices.to_owned(),
            variants: HashMap::new(),
        });
        Pipeline(self.pipelines.len() - 1)
    }
    pub fn apply_pipeline(&mut self, pipeline: &Pipeline) {
        self.pipeline = Some(pipeline.0);
        let pass = self.current_pass.map(|p| self.passes[p].as_ref().unwrap());
        let samples = pass.map_or(1, |p| p.samples as u64);
        let depth = pass.is_some_and(|p| p.depth.is_some());
        let entry = &mut self.pipelines[pipeline.0];
        let device = &self.device;
        let shader = &self.shaders[entry.shader];
        let params = entry.params;
        let vertices = &entry.vertices;
        let state = entry.variants.entry((samples, depth)).or_insert_with(|| {
            let desc = m::RenderPipelineDescriptor::new();
            desc.set_vertex_function(Some(&shader.vertex));
            desc.set_fragment_function(Some(&shader.fragment));
            desc.set_vertex_descriptor(Some(vertices));
            desc.set_sample_count(samples);
            if depth {
                desc.set_depth_attachment_pixel_format(m::MTLPixelFormat::Depth32Float_Stencil8);
                desc.set_stencil_attachment_pixel_format(m::MTLPixelFormat::Depth32Float_Stencil8);
            }
            let color = desc.color_attachments().object_at(0).unwrap();
            color.set_pixel_format(m::MTLPixelFormat::RGBA8Unorm);
            color.set_write_mask(color_mask(params.color_write));
            if let Some(blend) = params.color_blend {
                color.set_blending_enabled(true);
                color.set_rgb_blend_operation(equation(blend.equation));
                color.set_source_rgb_blend_factor(blend_factor(blend.sfactor));
                color.set_destination_rgb_blend_factor(blend_factor(blend.dfactor));
                let alpha = params.alpha_blend.unwrap_or(blend);
                color.set_alpha_blend_operation(equation(alpha.equation));
                color.set_source_alpha_blend_factor(blend_factor(alpha.sfactor));
                color.set_destination_alpha_blend_factor(blend_factor(alpha.dfactor));
            }
            device
                .new_render_pipeline_state(&desc)
                .expect("Metal pipeline creation failed")
        });
        let encoder = self
            .encoder
            .as_ref()
            .expect("draw outside Metal render pass");
        encoder.set_render_pipeline_state(state);
        let depth_desc = m::DepthStencilDescriptor::new();
        depth_desc.set_depth_compare_function(comparison(entry.params.depth_test));
        depth_desc.set_depth_write_enabled(entry.params.depth_write);
        if let Some(stencil) = entry.params.stencil_test {
            depth_desc.set_front_face_stencil(Some(&stencil_face(stencil.front)));
            depth_desc.set_back_face_stencil(Some(&stencil_face(stencil.back)));
            encoder.set_stencil_front_back_reference_value(
                stencil.front.test_ref as u32,
                stencil.back.test_ref as u32,
            );
        }
        encoder.set_depth_stencil_state(&self.device.new_depth_stencil_state(&depth_desc));
        encoder.set_cull_mode(cull(entry.params.cull_face));
        encoder.set_front_facing_winding(match entry.params.front_face_order {
            FrontFaceOrder::Clockwise => m::MTLWinding::Clockwise,
            FrontFaceOrder::CounterClockwise => m::MTLWinding::CounterClockwise,
        });
    }
    pub fn set_cull_face(&mut self, face: CullFace) {
        if let Some(e) = &self.encoder {
            e.set_cull_mode(cull(face));
        }
    }
    pub fn set_color_write(&mut self, mask: ColorMask) {
        self.modify_pipeline(|p| p.color_write = mask);
    }
    pub fn set_blend(&mut self, color: Option<BlendState>, alpha: Option<BlendState>) {
        self.modify_pipeline(|p| {
            p.color_blend = color;
            p.alpha_blend = alpha;
        });
    }
    pub fn set_stencil(&mut self, stencil: Option<StencilState>) {
        self.modify_pipeline(|p| p.stencil_test = stencil);
    }
    fn modify_pipeline(&mut self, f: impl FnOnce(&mut PipelineParams)) {
        if let Some(index) = self.pipeline {
            f(&mut self.pipelines[index].params);
            self.pipelines[index].variants.clear();
            self.apply_pipeline(&Pipeline(index));
        }
    }
    pub fn apply_viewport(&mut self, x: i32, y: i32, w: i32, h: i32) {
        if let Some(e) = &self.encoder {
            e.set_viewport(m::MTLViewport {
                originX: x as f64,
                originY: y as f64,
                width: w as f64,
                height: h as f64,
                znear: 0.,
                zfar: 1.,
            });
        }
    }
    pub fn apply_scissor_rect(&mut self, x: i32, y: i32, w: i32, h: i32) {
        let color = self.current_pass.map_or(self.default_color, |p| {
            self.passes[p].as_ref().unwrap().color
        });
        let x0 = x.max(0).min(color.width as i32) as u64;
        let y0 = y.max(0).min(color.height as i32) as u64;
        let x1 = (x + w).max(x0 as i32).min(color.width as i32) as u64;
        let y1 = (y + h).max(y0 as i32).min(color.height as i32) as u64;
        if let Some(e) = &self.encoder {
            e.set_scissor_rect(m::MTLScissorRect {
                x: x0,
                y: y0,
                width: x1 - x0,
                height: y1 - y0,
            });
        }
    }
    pub fn apply_bindings(&mut self, bindings: &Bindings) {
        let e = self.encoder.as_ref().unwrap();
        for (index, buffer) in bindings.vertex_buffers.iter().enumerate() {
            e.set_vertex_buffer(
                index as u64 + 1,
                self.buffers[buffer.gl_buf as usize].as_deref(),
                0,
            );
        }
        self.index = Some(bindings.index_buffer);
        for (index, texture) in bindings.images.iter().enumerate() {
            let entry = self.textures[texture.texture as usize]
                .as_ref()
                .expect("binding deleted Metal texture");
            e.set_fragment_texture(index as u64, Some(&entry.image));
            e.set_fragment_sampler_state(index as u64, Some(&entry.sampler));
            e.set_vertex_texture(index as u64, Some(&entry.image));
            e.set_vertex_sampler_state(index as u64, Some(&entry.sampler));
        }
    }
    pub fn apply_uniforms_from_bytes(&mut self, ptr: *const u8, size: usize) {
        let layout = &self.shaders[self.pipelines[self.pipeline.unwrap()].shader].layout;
        if layout.uniforms.is_empty() {
            return;
        }
        let bytes =
            super::metal_shader::pack(layout, unsafe { std::slice::from_raw_parts(ptr, size) });
        let buffer = self.device.new_buffer_with_data(
            bytes.as_ptr() as *const c_void,
            bytes.len() as u64,
            m::MTLResourceOptions::StorageModeShared,
        );
        let e = self.encoder.as_ref().unwrap();
        e.set_vertex_buffer(0, Some(&buffer), 0);
        e.set_fragment_buffer(0, Some(&buffer), 0);
        self.transient.push(buffer);
    }
    pub fn new_buffer(
        &mut self,
        kind: BufferType,
        index_type: Option<IndexType>,
        size: usize,
        ptr: Option<*const u8>,
    ) -> Buffer {
        let buffer = match ptr {
            Some(ptr) => self.device.new_buffer_with_data(
                ptr as _,
                size as u64,
                m::MTLResourceOptions::StorageModeShared,
            ),
            None => self
                .device
                .new_buffer(size as u64, m::MTLResourceOptions::StorageModeShared),
        };
        self.buffers.push(Some(buffer));
        Buffer {
            gl_buf: (self.buffers.len() - 1) as u32,
            buffer_type: kind,
            index_type,
            size,
        }
    }
    pub fn update_buffer(&mut self, index: usize, ptr: *const u8, size: usize) {
        let previous = self.buffers[index].as_ref().unwrap();
        let next = self
            .device
            .new_buffer(previous.length(), m::MTLResourceOptions::StorageModeShared);
        unsafe {
            std::ptr::copy_nonoverlapping(
                previous.contents() as *const u8,
                next.contents() as *mut u8,
                previous.length() as usize,
            );
            std::ptr::copy_nonoverlapping(ptr, next.contents() as *mut u8, size);
        }
        self.transient
            .push(self.buffers[index].replace(next).unwrap());
    }
    pub fn delete_buffer(&mut self, index: usize) {
        if let Some(buffer) = self.buffers[index].take() {
            self.transient.push(buffer);
        }
    }
    pub fn new_texture(
        &mut self,
        bytes: Option<&[u8]>,
        params: TextureParams,
        _access: TextureAccess,
    ) -> usize {
        let desc = m::TextureDescriptor::new();
        desc.set_width(params.width as u64);
        desc.set_height(params.height as u64);
        desc.set_pixel_format(if params.format == TextureFormat::Depth {
            m::MTLPixelFormat::Depth32Float_Stencil8
        } else {
            m::MTLPixelFormat::RGBA8Unorm
        });
        desc.set_storage_mode(if params.format == TextureFormat::Depth {
            m::MTLStorageMode::Private
        } else {
            m::MTLStorageMode::Shared
        });
        desc.set_usage(m::MTLTextureUsage::ShaderRead | m::MTLTextureUsage::RenderTarget);
        if params.mipmap_filter != MipmapFilter::None {
            desc.set_mipmap_level_count(
                (params.width.max(params.height) as f64).log2().floor() as u64 + 1,
            );
        }
        let image = self.device.new_texture(&desc);
        let sampler = self.sampler(params);
        self.textures.push(Some(MetalTexture {
            image,
            sampler,
            params,
        }));
        let index = self.textures.len() - 1;
        if let Some(bytes) = bytes {
            self.update_texture_part(
                index,
                0,
                0,
                params.width as i32,
                params.height as i32,
                bytes,
                params.format,
            );
        }
        index
    }
    fn sampler(&self, params: TextureParams) -> m::SamplerState {
        let desc = m::SamplerDescriptor::new();
        let filter = if params.filter == FilterMode::Nearest {
            m::MTLSamplerMinMagFilter::Nearest
        } else {
            m::MTLSamplerMinMagFilter::Linear
        };
        desc.set_min_filter(filter);
        desc.set_mag_filter(filter);
        desc.set_mip_filter(match params.mipmap_filter {
            MipmapFilter::None => m::MTLSamplerMipFilter::NotMipmapped,
            MipmapFilter::Nearest => m::MTLSamplerMipFilter::Nearest,
            MipmapFilter::Linear => m::MTLSamplerMipFilter::Linear,
        });
        let wrap = match params.wrap {
            TextureWrap::Repeat => m::MTLSamplerAddressMode::Repeat,
            TextureWrap::Mirror => m::MTLSamplerAddressMode::MirrorRepeat,
            TextureWrap::Clamp => m::MTLSamplerAddressMode::ClampToEdge,
        };
        desc.set_address_mode_s(wrap);
        desc.set_address_mode_t(wrap);
        self.device.new_sampler(&desc)
    }
    pub fn set_texture_filter(&mut self, index: usize, filter: FilterMode) {
        let mut p = self.textures[index].as_ref().unwrap().params;
        p.filter = filter;
        let sampler = self.sampler(p);
        let e = self.textures[index].as_mut().unwrap();
        e.params = p;
        e.sampler = sampler;
    }
    pub fn set_texture_wrap(&mut self, index: usize, wrap: TextureWrap) {
        let mut p = self.textures[index].as_ref().unwrap().params;
        p.wrap = wrap;
        let sampler = self.sampler(p);
        let e = self.textures[index].as_mut().unwrap();
        e.params = p;
        e.sampler = sampler;
    }
    pub fn resize_texture(
        &mut self,
        index: usize,
        w: u32,
        h: u32,
        _format: TextureFormat,
        bytes: Option<&[u8]>,
    ) {
        self.finish();
        let mut p = self.textures[index].as_ref().unwrap().params;
        p.width = w;
        p.height = h;
        let new = self.new_texture(bytes, p, TextureAccess::Static);
        self.textures[index] = self.textures[new].take();
    }
    pub fn update_texture_part(
        &mut self,
        index: usize,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        bytes: &[u8],
        format: TextureFormat,
    ) {
        self.finish(); // Texture updates must not race previous GPU readers.
        let rgba: Vec<u8> = match format {
            TextureFormat::RGBA8 => bytes.to_vec(),
            TextureFormat::RGB8 => bytes
                .chunks_exact(3)
                .flat_map(|c| [c[0], c[1], c[2], 255])
                .collect(),
            TextureFormat::Alpha => bytes.iter().flat_map(|&a| [255, 255, 255, a]).collect(),
            TextureFormat::LuminanceAlpha => bytes
                .chunks_exact(2)
                .flat_map(|c| [c[0], c[0], c[0], c[1]])
                .collect(),
            TextureFormat::Depth => panic!("CPU upload to a Metal depth attachment"),
        };
        self.image(index).replace_region(
            m::MTLRegion::new_2d(x as u64, y as u64, w as u64, h as u64),
            0,
            rgba.as_ptr() as _,
            w as u64 * 4,
        );
    }
    pub fn read_texture_pixels(
        &mut self,
        index: usize,
        w: u32,
        h: u32,
        format: TextureFormat,
        bytes: &mut [u8],
    ) {
        self.finish();
        let mut rgba = vec![0u8; w as usize * h as usize * 4];
        self.image(index).get_bytes(
            rgba.as_mut_ptr() as _,
            w as u64 * 4,
            m::MTLRegion::new_2d(0, 0, w as u64, h as u64),
            0,
        );
        assert_eq!(bytes.len(), format.size(w, h) as usize);
        match format {
            TextureFormat::RGBA8 => bytes.copy_from_slice(&rgba),
            TextureFormat::RGB8 => {
                for (dst, src) in bytes.chunks_exact_mut(3).zip(rgba.chunks_exact(4)) {
                    dst.copy_from_slice(&src[..3]);
                }
            }
            TextureFormat::Alpha => {
                for (dst, src) in bytes.iter_mut().zip(rgba.chunks_exact(4)) {
                    *dst = src[3];
                }
            }
            TextureFormat::LuminanceAlpha => {
                for (dst, src) in bytes.chunks_exact_mut(2).zip(rgba.chunks_exact(4)) {
                    dst.copy_from_slice(&[src[0], src[3]]);
                }
            }
            TextureFormat::Depth => panic!("CPU read of a Metal depth attachment"),
        }
    }
    /// Read the native RGBA backing, including RGB8 render targets. The video
    /// encoder consumes four channels regardless of the public texture format.
    pub fn read_rgba(&mut self, texture: Texture, bytes: &mut [u8]) {
        assert_eq!(
            bytes.len(),
            texture.width as usize * texture.height as usize * 4
        );
        self.finish();
        self.image(texture.texture as usize).get_bytes(
            bytes.as_mut_ptr() as _,
            texture.width as u64 * 4,
            m::MTLRegion::new_2d(0, 0, texture.width as u64, texture.height as u64),
            0,
        );
    }
    pub fn delete_texture(&mut self, index: usize) {
        self.finish();
        self.textures[index].take();
    }
    pub fn generate_mipmap(&mut self, index: usize) {
        self.end_render_pass();
        let image = self.image(index).to_owned();
        let encoder = self.command().new_blit_command_encoder();
        encoder.generate_mipmaps(&image);
        encoder.end_encoding();
    }
    fn depth_attachment(&self, color: Texture, samples: u32) -> m::Texture {
        let desc = m::TextureDescriptor::new();
        desc.set_width(color.width as u64);
        desc.set_height(color.height as u64);
        desc.set_pixel_format(m::MTLPixelFormat::Depth32Float_Stencil8);
        desc.set_storage_mode(m::MTLStorageMode::Private);
        desc.set_usage(m::MTLTextureUsage::RenderTarget);
        if samples > 1 {
            desc.set_texture_type(m::MTLTextureType::D2Multisample);
            desc.set_sample_count(samples as u64);
        }
        self.device.new_texture(&desc)
    }
    pub fn new_render_pass(&mut self, color: Texture, depth: Option<Texture>) -> usize {
        let depth = Some(
            depth
                .map(|texture| self.image(texture.texture as usize).to_owned())
                .unwrap_or_else(|| self.depth_attachment(color, 1)),
        );
        self.passes.push(Some(MetalPass {
            color,
            depth,
            samples: 1,
            msaa: None,
        }));
        self.passes.len() - 1
    }
    pub fn new_msaa_pass(&mut self, color: Texture, requested: u32) -> usize {
        let samples = [8, 4, 2, 1]
            .iter()
            .copied()
            .find(|&s| s <= requested && self.device.supports_texture_sample_count(s as u64))
            .unwrap_or(1);
        let pass = self.new_render_pass(color, None);
        if samples > 1 {
            let desc = m::TextureDescriptor::new();
            desc.set_width(color.width as u64);
            desc.set_height(color.height as u64);
            desc.set_pixel_format(m::MTLPixelFormat::RGBA8Unorm);
            desc.set_texture_type(m::MTLTextureType::D2Multisample);
            desc.set_sample_count(samples as u64);
            desc.set_storage_mode(m::MTLStorageMode::Private);
            desc.set_usage(m::MTLTextureUsage::RenderTarget);
            let depth = self.depth_attachment(color, samples);
            let p = self.passes[pass].as_mut().unwrap();
            p.samples = samples;
            p.msaa = Some(self.device.new_texture(&desc));
            p.depth = Some(depth);
        }
        pass
    }
    pub fn begin_pass(&mut self, pass: Option<RenderPass>, action: PassAction) {
        self.end_render_pass();
        self.current_pass = pass.map(|p| p.0);
        let color = self.current_pass.map_or(self.default_color, |p| {
            self.passes[p].as_ref().unwrap().color
        });
        let desc = m::RenderPassDescriptor::new();
        let attachment = desc.color_attachments().object_at(0).unwrap();
        attachment.set_texture(Some(self.image(color.texture as usize)));
        attachment.set_store_action(m::MTLStoreAction::Store);
        if let Some(p) = self.current_pass.map(|p| self.passes[p].as_ref().unwrap()) {
            if let Some(msaa) = &p.msaa {
                attachment.set_texture(Some(msaa));
                attachment.set_resolve_texture(Some(self.image(color.texture as usize)));
                attachment.set_store_action(m::MTLStoreAction::StoreAndMultisampleResolve);
            }
            if let Some(depth) = &p.depth {
                desc.depth_attachment().unwrap().set_texture(Some(depth));
                desc.depth_attachment()
                    .unwrap()
                    .set_store_action(m::MTLStoreAction::Store);
                desc.stencil_attachment().unwrap().set_texture(Some(depth));
                desc.stencil_attachment()
                    .unwrap()
                    .set_store_action(m::MTLStoreAction::Store);
            }
        }
        match action {
            PassAction::Nothing => {
                attachment.set_load_action(m::MTLLoadAction::Load);
                desc.depth_attachment()
                    .unwrap()
                    .set_load_action(m::MTLLoadAction::Load);
                desc.stencil_attachment()
                    .unwrap()
                    .set_load_action(m::MTLLoadAction::Load);
            }
            PassAction::Clear {
                color,
                depth,
                stencil,
            } => {
                attachment.set_load_action(if color.is_some() {
                    m::MTLLoadAction::Clear
                } else {
                    m::MTLLoadAction::Load
                });
                if let Some((r, g, b, a)) = color {
                    attachment.set_clear_color(m::MTLClearColor::new(
                        r as f64, g as f64, b as f64, a as f64,
                    ));
                }
                desc.depth_attachment()
                    .unwrap()
                    .set_load_action(if depth.is_some() {
                        m::MTLLoadAction::Clear
                    } else {
                        m::MTLLoadAction::Load
                    });
                if let Some(depth) = depth {
                    desc.depth_attachment()
                        .unwrap()
                        .set_clear_depth(depth as f64);
                }
                desc.stencil_attachment()
                    .unwrap()
                    .set_load_action(if stencil.is_some() {
                        m::MTLLoadAction::Clear
                    } else {
                        m::MTLLoadAction::Load
                    });
                if let Some(stencil) = stencil {
                    desc.stencil_attachment()
                        .unwrap()
                        .set_clear_stencil(stencil as u32);
                }
            }
        }
        self.encoder = Some(self.command().new_render_command_encoder(&desc).to_owned());
        self.pipeline = None;
        self.apply_viewport(0, 0, color.width as i32, color.height as i32);
        self.apply_scissor_rect(0, 0, color.width as i32, color.height as i32);
    }
    pub fn clear(
        &mut self,
        color: Option<(f32, f32, f32, f32)>,
        depth: Option<f32>,
        stencil: Option<i32>,
    ) {
        self.begin_pass(
            self.current_pass.map(RenderPass),
            PassAction::Clear {
                color,
                depth,
                stencil,
            },
        );
    }
    pub fn end_render_pass(&mut self) {
        if let Some(e) = self.encoder.take() {
            e.end_encoding();
        }
    }
    pub fn commit_frame(&mut self) {
        self.finish();
    }
    pub fn draw(&mut self, base: i32, count: i32, instances: i32) {
        let index = self.index.unwrap();
        let kind = match index.index_type.unwrap() {
            IndexType::Short => m::MTLIndexType::UInt16,
            IndexType::Int => m::MTLIndexType::UInt32,
            IndexType::Byte => panic!("Metal requires 16/32-bit indices"),
        };
        let primitive = match self.pipelines[self.pipeline.unwrap()].params.primitive_type {
            PrimitiveType::Triangles => m::MTLPrimitiveType::Triangle,
            PrimitiveType::Lines => m::MTLPrimitiveType::Line,
        };
        self.encoder
            .as_ref()
            .unwrap()
            .draw_indexed_primitives_instanced(
                primitive,
                count as u64,
                kind,
                self.buffers[index.gl_buf as usize].as_ref().unwrap(),
                base as u64 * index.index_type.unwrap().size() as u64,
                instances as u64,
            );
    }
    pub fn copy_texture(&mut self, source: usize, destination: usize) {
        self.end_render_pass();
        let src = self.image(source).to_owned();
        let dst = self.image(destination).to_owned();
        if (src.width(), src.height()) != (dst.width(), dst.height()) {
            if self.scale_pipeline.is_none() {
                let source = r#"#include <metal_stdlib>
using namespace metal;
struct Varying { float4 position [[position]]; float2 uv; };
vertex Varying resize_vertex(uint id [[vertex_id]]) {
    const float2 positions[3] = {float2(-1,-1),float2(3,-1),float2(-1,3)};
    float2 p=positions[id]; Varying out; out.position=float4(p,0,1);
    out.uv=float2((p.x+1)*0.5,(1-p.y)*0.5); return out;
}
fragment float4 resize_fragment(Varying in [[stage_in]], texture2d<float> image [[texture(0)]]) {
    constexpr sampler linearSampler(coord::normalized,address::clamp_to_edge,filter::linear);
    return image.sample(linearSampler,in.uv);
}"#;
                let library = self
                    .device
                    .new_library_with_source(source, &m::CompileOptions::new())
                    .expect("Metal resize shader");
                let desc = m::RenderPipelineDescriptor::new();
                desc.set_vertex_function(Some(
                    &library.get_function("resize_vertex", None).unwrap(),
                ));
                desc.set_fragment_function(Some(
                    &library.get_function("resize_fragment", None).unwrap(),
                ));
                desc.color_attachments()
                    .object_at(0)
                    .unwrap()
                    .set_pixel_format(m::MTLPixelFormat::RGBA8Unorm);
                self.scale_pipeline = Some(
                    self.device
                        .new_render_pipeline_state(&desc)
                        .expect("Metal resize pipeline"),
                );
            }
            let desc = m::RenderPassDescriptor::new();
            let attachment = desc.color_attachments().object_at(0).unwrap();
            attachment.set_texture(Some(&dst));
            attachment.set_load_action(m::MTLLoadAction::DontCare);
            attachment.set_store_action(m::MTLStoreAction::Store);
            let command = self.command().to_owned();
            let encoder = command.new_render_command_encoder(&desc);
            encoder.set_render_pipeline_state(self.scale_pipeline.as_ref().unwrap());
            encoder.set_fragment_texture(0, Some(&src));
            encoder.draw_primitives(m::MTLPrimitiveType::Triangle, 0, 3);
            encoder.end_encoding();
            return;
        }
        let e = self.command().new_blit_command_encoder();
        e.copy_from_texture(
            &src,
            0,
            0,
            m::MTLOrigin { x: 0, y: 0, z: 0 },
            m::MTLSize {
                width: src.width(),
                height: src.height(),
                depth: 1,
            },
            &dst,
            0,
            0,
            m::MTLOrigin { x: 0, y: 0, z: 0 },
        );
        e.end_encoding();
    }
    pub fn delete_render_pass(&mut self, index: usize) {
        self.finish();
        self.passes[index].take();
    }
}
impl Drop for MetalContext {
    fn drop(&mut self) {
        self.finish();
    }
}

fn color_mask(mask: ColorMask) -> m::MTLColorWriteMask {
    let mut flags = m::MTLColorWriteMask::empty();
    for (enabled, flag) in [
        (mask.0, m::MTLColorWriteMask::Red),
        (mask.1, m::MTLColorWriteMask::Green),
        (mask.2, m::MTLColorWriteMask::Blue),
        (mask.3, m::MTLColorWriteMask::Alpha),
    ] {
        if enabled {
            flags |= flag;
        }
    }
    flags
}
fn cull(face: CullFace) -> m::MTLCullMode {
    match face {
        CullFace::Nothing => m::MTLCullMode::None,
        CullFace::Front => m::MTLCullMode::Front,
        CullFace::Back => m::MTLCullMode::Back,
    }
}
fn equation(e: Equation) -> m::MTLBlendOperation {
    match e {
        Equation::Add => m::MTLBlendOperation::Add,
        Equation::Subtract => m::MTLBlendOperation::Subtract,
        Equation::ReverseSubtract => m::MTLBlendOperation::ReverseSubtract,
    }
}
fn comparison(c: Comparison) -> m::MTLCompareFunction {
    match c {
        Comparison::Never => m::MTLCompareFunction::Never,
        Comparison::Less => m::MTLCompareFunction::Less,
        Comparison::LessOrEqual => m::MTLCompareFunction::LessEqual,
        Comparison::Greater => m::MTLCompareFunction::Greater,
        Comparison::GreaterOrEqual => m::MTLCompareFunction::GreaterEqual,
        Comparison::Equal => m::MTLCompareFunction::Equal,
        Comparison::NotEqual => m::MTLCompareFunction::NotEqual,
        Comparison::Always => m::MTLCompareFunction::Always,
    }
}
fn blend_factor(f: BlendFactor) -> m::MTLBlendFactor {
    match f {
        BlendFactor::Zero => m::MTLBlendFactor::Zero,
        BlendFactor::One => m::MTLBlendFactor::One,
        BlendFactor::Value(BlendValue::SourceColor) => m::MTLBlendFactor::SourceColor,
        BlendFactor::Value(BlendValue::SourceAlpha) => m::MTLBlendFactor::SourceAlpha,
        BlendFactor::Value(BlendValue::DestinationColor) => m::MTLBlendFactor::DestinationColor,
        BlendFactor::Value(BlendValue::DestinationAlpha) => m::MTLBlendFactor::DestinationAlpha,
        BlendFactor::OneMinusValue(BlendValue::SourceColor) => {
            m::MTLBlendFactor::OneMinusSourceColor
        }
        BlendFactor::OneMinusValue(BlendValue::SourceAlpha) => {
            m::MTLBlendFactor::OneMinusSourceAlpha
        }
        BlendFactor::OneMinusValue(BlendValue::DestinationColor) => {
            m::MTLBlendFactor::OneMinusDestinationColor
        }
        BlendFactor::OneMinusValue(BlendValue::DestinationAlpha) => {
            m::MTLBlendFactor::OneMinusDestinationAlpha
        }
        BlendFactor::SourceAlphaSaturate => m::MTLBlendFactor::SourceAlphaSaturated,
    }
}
fn vertex_format(f: VertexFormat) -> m::MTLVertexFormat {
    match f {
        VertexFormat::Float1 => m::MTLVertexFormat::Float,
        VertexFormat::Float2 => m::MTLVertexFormat::Float2,
        VertexFormat::Float3 => m::MTLVertexFormat::Float3,
        VertexFormat::Float4 | VertexFormat::Mat4 => m::MTLVertexFormat::Float4,
        VertexFormat::Byte1 => m::MTLVertexFormat::UChar,
        VertexFormat::Byte2 => m::MTLVertexFormat::UChar2,
        VertexFormat::Byte3 => m::MTLVertexFormat::UChar3,
        VertexFormat::Byte4 => m::MTLVertexFormat::UChar4,
        VertexFormat::Short1 => m::MTLVertexFormat::UShort,
        VertexFormat::Short2 => m::MTLVertexFormat::UShort2,
        VertexFormat::Short3 => m::MTLVertexFormat::UShort3,
        VertexFormat::Short4 => m::MTLVertexFormat::UShort4,
        VertexFormat::Int1 => m::MTLVertexFormat::UInt,
        VertexFormat::Int2 => m::MTLVertexFormat::UInt2,
        VertexFormat::Int3 => m::MTLVertexFormat::UInt3,
        VertexFormat::Int4 => m::MTLVertexFormat::UInt4,
    }
}

fn stencil_face(face: StencilFaceState) -> m::StencilDescriptor {
    let desc = m::StencilDescriptor::new();
    desc.set_stencil_compare_function(match face.test_func {
        CompareFunc::Always => m::MTLCompareFunction::Always,
        CompareFunc::Never => m::MTLCompareFunction::Never,
        CompareFunc::Less => m::MTLCompareFunction::Less,
        CompareFunc::LessOrEqual => m::MTLCompareFunction::LessEqual,
        CompareFunc::Greater => m::MTLCompareFunction::Greater,
        CompareFunc::GreaterOrEqual => m::MTLCompareFunction::GreaterEqual,
        CompareFunc::Equal => m::MTLCompareFunction::Equal,
        CompareFunc::NotEqual => m::MTLCompareFunction::NotEqual,
    });
    desc.set_stencil_failure_operation(stencil_op(face.fail_op));
    desc.set_depth_failure_operation(stencil_op(face.depth_fail_op));
    desc.set_depth_stencil_pass_operation(stencil_op(face.pass_op));
    desc.set_read_mask(face.test_mask);
    desc.set_write_mask(face.write_mask);
    desc
}
fn stencil_op(op: StencilOp) -> m::MTLStencilOperation {
    match op {
        StencilOp::Keep => m::MTLStencilOperation::Keep,
        StencilOp::Zero => m::MTLStencilOperation::Zero,
        StencilOp::Replace => m::MTLStencilOperation::Replace,
        StencilOp::Invert => m::MTLStencilOperation::Invert,
        StencilOp::IncrementClamp => m::MTLStencilOperation::IncrementClamp,
        StencilOp::DecrementClamp => m::MTLStencilOperation::DecrementClamp,
        StencilOp::IncrementWrap => m::MTLStencilOperation::IncrementWrap,
        StencilOp::DecrementWrap => m::MTLStencilOperation::DecrementWrap,
    }
}
