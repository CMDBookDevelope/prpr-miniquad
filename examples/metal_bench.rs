//! Native allocation benchmark; these are synthetic draw timings, not chart FPS.
//! Build once with --release --features metal-bench, then run the same binary
//! with MINIQUAD_METAL_LEGACY_DRAW=1 and =0. The legacy code is feature-gated.
#[cfg(feature = "metal-bench")]
use miniquad::*;

#[cfg(feature = "metal-bench")]
struct Bench;

#[cfg(feature = "metal-bench")]
impl EventHandler for Bench {
    fn update(&mut self, _: &mut Context) {}

    fn draw(&mut self, ctx: &mut Context) {
        let source = Texture::new_render_texture(
            ctx,
            TextureParams {
                width: 1280,
                height: 720,
                ..Default::default()
            },
        );
        let pass = RenderPass::new(ctx, source, None);
        let shader = Shader::new(
            ctx,
            r#"#version 450 core
layout(location=0) in vec2 position;
void main() { gl_Position=vec4(position,0,1); }"#,
            r#"#version 450 core
layout(set=0,binding=0) uniform U { vec4 tint; };
layout(location=0) out vec4 color;
void main() { color=tint; }"#,
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
        let buffer = Buffer::stream(ctx, BufferType::VertexBuffer, 256 * 1024);
        let mut vertices = Vec::new();
        for i in 0..16 {
            let x = -0.9 + (i % 4) as f32 * 0.4;
            let y = -0.9 + (i / 4) as f32 * 0.4;
            vertices.extend_from_slice(&[[x, y], [x + 0.04, y], [x, y + 0.04]]);
        }
        let indices = Buffer::immutable(
            ctx,
            BufferType::IndexBuffer,
            &(0u16..vertices.len() as u16).collect::<Vec<_>>(),
        );
        let bindings = Bindings {
            vertex_buffers: vec![buffer],
            index_buffer: indices,
            images: vec![],
        };
        let mode = if std::env::var("MINIQUAD_METAL_LEGACY_DRAW").is_ok_and(|v| v == "1") {
            "legacy"
        } else {
            "optimized"
        };
        let gpu = ctx.metal_context().unwrap().device.name().to_owned();
        for partial in [false, true] {
            for _ in 0..6 {
                draw_frame(ctx, pass, &pipeline, &bindings, &vertices, partial);
            }
            ctx.metal_context().unwrap().finish();
            ctx.metal_context().unwrap().reset_bench_stats();
            let start = std::time::Instant::now();
            let mut encoding = std::time::Duration::ZERO;
            const FRAMES: u32 = 32;
            for _ in 0..FRAMES {
                encoding += draw_frame(ctx, pass, &pipeline, &bindings, &vertices, partial);
            }
            ctx.metal_context().unwrap().finish();
            let elapsed = start.elapsed();
            let stats = ctx.metal_context().unwrap().bench_stats();
            println!("{{\"benchmark\":\"metal_draw_allocations\",\"mode\":\"{}\",\"gpu\":{:?},\"workload\":\"{}\",\"width\":1280,\"height\":720,\"frames\":{},\"draws_per_frame\":128,\"declared_vertex_capacity\":262144,\"uploaded_vertex_bytes\":384,\"encoding_ms_per_frame\":{:.6},\"total_ms_per_frame\":{:.6},\"buffers_created\":{},\"buffers_reused\":{},\"buffer_bytes_copied\":{},\"uniform_buffers\":{},\"inline_uniforms\":{},\"depth_states\":{},\"pipeline_states\":{}}}",
                mode, gpu, if partial { "alternating_half_updates" } else { "full_updates" }, FRAMES,
                encoding.as_secs_f64() * 1000. / FRAMES as f64,
                elapsed.as_secs_f64() * 1000. / FRAMES as f64,
                stats.buffers_created, stats.buffers_reused, stats.buffer_bytes_copied,
                stats.uniform_buffers, stats.inline_uniforms, stats.depth_states, stats.pipeline_states);
        }
        ctx.request_quit();
    }
}

#[cfg(feature = "metal-bench")]
fn draw_frame(
    ctx: &mut Context,
    pass: RenderPass,
    pipeline: &Pipeline,
    bindings: &Bindings,
    vertices: &[[f32; 2]],
    partial: bool,
) -> std::time::Duration {
    let start = std::time::Instant::now();
    ctx.begin_pass(pass, PassAction::clear_color(0., 0., 0., 1.));
    for draw in 0..128 {
        let data = if partial && draw % 2 == 1 {
            &vertices[..vertices.len() / 2]
        } else {
            vertices
        };
        bindings.vertex_buffers[0].update(ctx, data);
        ctx.apply_pipeline(pipeline);
        ctx.apply_bindings(bindings);
        ctx.apply_uniforms(&[draw as f32 / 128., 0.25, 0.5, 1.]);
        ctx.draw(0, vertices.len() as i32, 1);
    }
    ctx.end_render_pass();
    let encoding = start.elapsed();
    // Both modes submit asynchronously, isolating draw allocation changes.
    ctx.metal_context().unwrap().submit().unwrap();
    encoding
}

#[cfg(feature = "metal-bench")]
fn main() {
    start(
        conf::Conf {
            headless: true,
            window_width: 1280,
            window_height: 720,
            platform: conf::Platform {
                rendering_backend: conf::RenderingBackend::Metal,
                ..Default::default()
            },
            ..Default::default()
        },
        |_| Box::new(Bench),
    );
}

#[cfg(not(feature = "metal-bench"))]
fn main() {
    eprintln!("Build on an Apple host with --features metal-bench");
}
