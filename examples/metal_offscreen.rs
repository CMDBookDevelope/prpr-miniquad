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
        let buffer = Buffer::stream(ctx, BufferType::VertexBuffer, 24);
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
        buffer.update(ctx, &[[4f32, 4.], [5., 4.], [4., 5.]]);
        ctx.apply_bindings(&Bindings {
            vertex_buffers: vec![buffer],
            index_buffer: indices,
            images: vec![],
        });
        ctx.apply_uniforms(&[0f32, 1., 0., 1.]);
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
        println!("Metal offscreen, MSAA, buffer snapshots and resize passed");
        ctx.request_quit();
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
