//! On an Apple host: cargo run --example metal_offscreen --features metal
#[cfg(feature = "metal")]
use miniquad::*;

#[cfg(feature = "metal")]
struct Smoke;
#[cfg(feature = "metal")]
impl EventHandler for Smoke {
    fn update(&mut self, _: &mut Context) {}
    fn draw(&mut self, ctx: &mut Context) {
        let source = Texture::new_render_texture(
            ctx,
            TextureParams {
                width: 32,
                height: 32,
                format: TextureFormat::RGB8,
                ..Default::default()
            },
        );
        let target = RenderPass::new_multisampled(ctx, source, 4);
        let vertex = r#"#version 450 core
layout(location=0) in vec2 position;
void main() { gl_Position = vec4(position,-0.5,1); }"#;
        let fragment = r#"#version 450 core
layout(set=0,binding=0) uniform U { vec4 tint; };
layout(location=0) out vec4 color;
void main() { color=tint; }"#;
        let shader = Shader::new(
            ctx,
            vertex,
            fragment,
            ShaderMeta {
                images: vec![],
                uniforms: UniformBlockLayout {
                    uniforms: vec![UniformDesc::new("tint", UniformType::Float4)],
                },
            },
        )
        .unwrap();
        let pipeline = Pipeline::new(
            ctx,
            &[BufferLayout::default()],
            &[VertexAttribute::new("position", VertexFormat::Float2)],
            shader,
        );
        let buffer = Buffer::stream(ctx, BufferType::VertexBuffer, 64 * 1024);
        buffer.update(ctx, &[[-1f32, -1.], [3., -1.], [-1., 3.]]);
        let indices = Buffer::immutable(ctx, BufferType::IndexBuffer, &[0u16, 1, 2]);
        ctx.begin_pass(target, PassAction::clear_color(0., 0., 0., 1.));
        ctx.apply_pipeline(&pipeline);
        ctx.apply_bindings(&Bindings {
            vertex_buffers: vec![buffer],
            index_buffer: indices,
            images: vec![],
        });
        ctx.apply_uniforms(&[1f32, 0., 0., 1.]);
        ctx.draw(0, 3, 1);
        // Reusing a stream buffer must preserve the earlier draw's geometry.
        // Partial writes must preserve the initialized third vertex (-1, 3).
        // Losing that suffix would introduce a vertex near the screen center.
        buffer.update(ctx, &[[4f32, 4.], [5., 4.]]);
        ctx.apply_bindings(&Bindings {
            vertex_buffers: vec![buffer],
            index_buffer: indices,
            images: vec![],
        });
        ctx.apply_uniforms(&[0f32, 1., 0., 1.]);
        ctx.draw(0, 3, 1);
        buffer.update(ctx, &[[4f32, 4.], [5., 4.], [4., 5.]]);
        ctx.apply_bindings(&Bindings {
            vertex_buffers: vec![buffer],
            index_buffer: indices,
            images: vec![],
        });
        ctx.apply_uniforms(&[0f32, 0., 1., 1.]);
        ctx.draw(0, 3, 1);
        ctx.end_render_pass();
        let small = Texture::new_render_texture(
            ctx,
            TextureParams {
                width: 8,
                height: 8,
                ..Default::default()
            },
        );
        let small_pass = RenderPass::new(ctx, small, None);
        ctx.copy_render_target(target, small_pass);
        let mut bytes = vec![0; 8 * 8 * 4];
        ctx.metal_context().unwrap().read_rgba(small, &mut bytes);
        assert!(bytes.chunks_exact(4).all(|pixel| pixel == [255, 0, 0, 255]));
        check_pipeline_states(ctx, source, target, &pipeline, buffer, indices);
        // A GL scissor at y=0 addresses the bottom-first texture rows. Sampling
        // that target must preserve the rows, including an unused empty sampler.
        buffer.update(ctx, &[[-1f32, -1.], [3., -1.], [-1., 3.]]);
        ctx.begin_pass(target, PassAction::clear_color(0., 0., 0., 1.));
        ctx.apply_pipeline(&pipeline);
        ctx.apply_scissor_rect(0, 0, 32, 16);
        ctx.apply_bindings(&Bindings {
            vertex_buffers: vec![buffer],
            index_buffer: indices,
            images: vec![],
        });
        ctx.apply_uniforms(&[1f32, 0., 0., 1.]);
        ctx.draw(0, 3, 1);
        ctx.end_render_pass();
        let sampling = Shader::new(
            ctx,
            r#"#version 450 core
layout(location=0) in vec2 position;
layout(location=0) out vec2 uv;
void main() { gl_Position=vec4(position,0,1); uv=(position+1)*0.5; }"#,
            r#"#version 450 core
layout(location=0) in vec2 uv;
layout(location=0) out vec4 color;
layout(set=0,binding=1) uniform sampler2D tex;
void main() { color=texture(tex,uv); }"#,
            ShaderMeta {
                images: vec!["tex".into(), "unused".into()],
                uniforms: UniformBlockLayout { uniforms: vec![] },
            },
        )
        .unwrap();
        let sampling_pipeline = Pipeline::new(
            ctx,
            &[BufferLayout::default()],
            &[VertexAttribute::new("position", VertexFormat::Float2)],
            sampling,
        );
        ctx.begin_pass(small_pass, PassAction::clear_color(0., 1., 0., 1.));
        ctx.apply_pipeline(&sampling_pipeline);
        ctx.apply_bindings(&Bindings {
            vertex_buffers: vec![buffer],
            index_buffer: indices,
            images: vec![source, Texture::empty()],
        });
        ctx.draw(0, 3, 1);
        ctx.end_render_pass();
        ctx.metal_context().unwrap().read_rgba(small, &mut bytes);
        assert!(bytes[..4 * 8 * 4]
            .chunks_exact(4)
            .all(|pixel| pixel == [255, 0, 0, 255]));
        assert!(bytes[4 * 8 * 4..]
            .chunks_exact(4)
            .all(|pixel| pixel == [0, 0, 0, 255]));
        check_export(ctx, source, target);
        println!("Metal offscreen, MSAA, partial/full buffer and uniform snapshots, resize, GPU export and async submission passed");
        ctx.request_quit();
    }
}

#[cfg(feature = "metal")]
fn check_pipeline_states(
    ctx: &mut Context,
    source: Texture,
    pass: RenderPass,
    pipeline: &Pipeline,
    buffer: Buffer,
    indices: Buffer,
) {
    buffer.update(ctx, &[[-1f32, -1.], [3., -1.], [-1., 3.]]);
    let bindings = Bindings {
        vertex_buffers: vec![buffer],
        index_buffer: indices,
        images: vec![],
    };
    // Pipeline::set_blend edits the public params directly. Applying it again
    // must select a different cached render state without stale blend flags.
    pipeline.set_blend(
        ctx,
        Some(BlendState::new(
            Equation::Add,
            BlendFactor::One,
            BlendFactor::One,
        )),
    );
    ctx.begin_pass(pass, PassAction::clear_color(0., 0., 1., 1.));
    ctx.apply_pipeline(pipeline);
    ctx.apply_bindings(&bindings);
    ctx.apply_uniforms(&[1f32, 0., 0., 0.]);
    ctx.draw(0, 3, 1);
    ctx.end_render_pass();
    let mut pixels = vec![0; 32 * 32 * 4];
    ctx.metal_context().unwrap().read_rgba(source, &mut pixels);
    assert!(pixels.chunks_exact(4).all(|p| p == [255, 0, 255, 255]));
    pipeline.set_blend(ctx, None);
    ctx.begin_pass(
        pass,
        PassAction::Clear {
            color: Some((0., 0., 1., 1.)),
            depth: Some(1.),
            stencil: Some(0),
        },
    );
    ctx.apply_pipeline(pipeline);
    ctx.apply_bindings(&bindings);
    ctx.set_color_write((false, true, true, true));
    ctx.apply_uniforms(&[1f32, 0., 0., 1.]);
    ctx.draw(0, 3, 1);
    ctx.set_color_write((true, true, true, true));
    ctx.end_render_pass();
    ctx.metal_context().unwrap().read_rgba(source, &mut pixels);
    assert!(pixels.chunks_exact(4).all(|p| p == [0, 0, 0, 255]));
    ctx.begin_pass(
        pass,
        PassAction::Clear {
            color: Some((0., 0., 0., 1.)),
            depth: Some(1.),
            stencil: Some(0),
        },
    );
    ctx.apply_pipeline(pipeline);
    ctx.apply_bindings(&bindings);
    let mut face = StencilFaceState {
        fail_op: StencilOp::Keep,
        depth_fail_op: StencilOp::Keep,
        pass_op: StencilOp::Replace,
        test_func: CompareFunc::Always,
        test_ref: 3,
        test_mask: 0xff,
        write_mask: 0xff,
    };
    for reference in [3, 7] {
        face.test_ref = reference;
        ctx.set_stencil(Some(StencilState {
            front: face,
            back: face,
        }));
        ctx.apply_uniforms(&[1f32, 0., 0., 1.]);
        ctx.draw(0, 3, 1);
    }
    face.pass_op = StencilOp::Keep;
    face.test_func = CompareFunc::Equal;
    ctx.set_stencil(Some(StencilState {
        front: face,
        back: face,
    }));
    ctx.apply_uniforms(&[0f32, 1., 0., 1.]);
    ctx.draw(0, 3, 1);
    ctx.set_stencil(None);
    ctx.end_render_pass();
    ctx.metal_context().unwrap().read_rgba(source, &mut pixels);
    assert!(pixels.chunks_exact(4).all(|p| p == [0, 255, 0, 255]));
}

#[cfg(feature = "metal")]
fn native_target(ctx: &mut Context, format: metal_rs::MTLPixelFormat) -> metal_rs::Texture {
    let desc = metal_rs::TextureDescriptor::new();
    desc.set_width(32);
    desc.set_height(32);
    desc.set_pixel_format(format);
    desc.set_storage_mode(metal_rs::MTLStorageMode::Shared);
    desc.set_usage(metal_rs::MTLTextureUsage::RenderTarget);
    ctx.metal_context().unwrap().device.new_texture(&desc)
}

#[cfg(feature = "metal")]
fn native_pixels(texture: &metal_rs::TextureRef) -> Vec<u8> {
    let mut pixels = vec![0; 32 * 32 * 4];
    texture.get_bytes(
        pixels.as_mut_ptr() as _,
        32 * 4,
        metal_rs::MTLRegion::new_2d(0, 0, 32, 32),
        0,
    );
    pixels
}

#[cfg(feature = "metal")]
fn check_export(ctx: &mut Context, source: Texture, pass: RenderPass) {
    use metal_rs::foreign_types::ForeignTypeRef;
    let rgba = native_target(ctx, metal_rs::MTLPixelFormat::RGBA8Unorm);
    let bgra = native_target(ctx, metal_rs::MTLPixelFormat::BGRA8Unorm);
    let metal = ctx.metal_context().unwrap();
    assert!(!metal.native_device().is_null());
    assert!(unsafe { metal.copy_to_native_texture(source, std::ptr::null_mut(), true) }.is_err());
    unsafe {
        metal
            .copy_to_native_texture(source, rgba.as_ptr() as _, false)
            .unwrap();
        metal
            .copy_to_native_texture(source, bgra.as_ptr() as _, true)
            .unwrap();
    }
    let token = metal.submit().unwrap();
    token.wait().unwrap();
    assert!(token.is_complete());
    let rgba_pixels = native_pixels(&rgba);
    let bgra_pixels = native_pixels(&bgra);
    for y in 0..32 {
        for x in 0..32 {
            let i = (y * 32 + x) * 4;
            let rgba_expected = if y < 16 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 0, 255]
            };
            let bgra_expected = if y >= 16 {
                [0, 0, 255, 255]
            } else {
                [0, 0, 0, 255]
            };
            assert_eq!(&rgba_pixels[i..i + 4], rgba_expected, "RGBA row {y}");
            assert_eq!(
                &bgra_pixels[i..i + 4],
                bgra_expected,
                "BGRA flipped row {y}"
            );
        }
    }
    // Reusing one source across commands must preserve each exported frame.
    // A fourth submission exercises the backend's three-batch bound; finish
    // then waits every outstanding token, not just the newest command.
    let mut pending = Vec::new();
    for color in [
        [255u8, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [255, 255, 0, 255],
    ] {
        let destination = native_target(ctx, metal_rs::MTLPixelFormat::BGRA8Unorm);
        ctx.begin_pass(
            pass,
            PassAction::clear_color(
                color[0] as f32 / 255.,
                color[1] as f32 / 255.,
                color[2] as f32 / 255.,
                1.,
            ),
        );
        ctx.end_render_pass();
        let metal = ctx.metal_context().unwrap();
        unsafe {
            metal
                .copy_to_native_texture(source, destination.as_ptr() as _, true)
                .unwrap();
        }
        pending.push((metal.submit().unwrap(), destination, color));
    }
    ctx.metal_context().unwrap().finish();
    for (token, destination, color) in pending {
        assert!(token.is_complete());
        token.wait().unwrap();
        let expected = [color[2], color[1], color[0], color[3]];
        assert!(native_pixels(&destination)
            .chunks_exact(4)
            .all(|p| p == expected));
    }
}
#[cfg(feature = "metal")]
fn main() {
    start(
        conf::Conf {
            headless: true,
            window_width: 32,
            window_height: 32,
            platform: conf::Platform {
                rendering_backend: conf::RenderingBackend::Metal,
                ..Default::default()
            },
            ..Default::default()
        },
        |_| Box::new(Smoke),
    );
}
#[cfg(not(feature = "metal"))]
fn main() {
    eprintln!("Enable the metal feature on an Apple host");
}
