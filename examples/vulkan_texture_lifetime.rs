//! Headless GPU regression for replacing a sampled texture during a frame.
//! Run with `cargo run --features vulkan,headless --example vulkan_texture_lifetime`.
//! Enable MINIQUAD_VULKAN_VALIDATION=1 when the Khronos layer is installed.
use miniquad::*;

struct Regression {
    pipeline: Pipeline,
    bindings: Bindings,
    target: Texture,
    pass: RenderPass,
}

impl Regression {
    fn new(ctx: &mut Context) -> Self {
        let vertices: [[f32; 4]; 4] = [
            [-0.5, -0.5, 0.0, 0.0],
            [0.5, -0.5, 1.0, 0.0],
            [0.5, 0.5, 1.0, 1.0],
            [-0.5, 0.5, 0.0, 1.0],
        ];
        let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];
        let shader = Shader::new(
            ctx,
            r#"#version 450 core
            layout(location=0) in vec2 pos;
            layout(location=1) in vec2 uv;
            layout(location=0) out vec2 texcoord;
            void main() { gl_Position = vec4(pos, 0.0, 1.0); texcoord = uv; }"#,
            r#"#version 450 core
            layout(location=0) in vec2 texcoord;
            layout(location=0) out vec4 color;
            layout(set=0,binding=0) uniform Uniforms { vec4 tint; };
            layout(set=0,binding=1) uniform sampler2D tex;
            layout(set=0,binding=2) uniform sampler2D stable;
            void main() {
                color = texture(tex, texcoord) * texture(stable, texcoord) * tint;
            }"#,
            ShaderMeta {
                images: vec!["tex".into(), "stable".into()],
                uniforms: UniformBlockLayout {
                    uniforms: vec![UniformDesc::new("tint", UniformType::Float4)],
                },
            },
        )
        .unwrap();
        let pipeline = Pipeline::new(
            ctx,
            &[BufferLayout::default()],
            &[
                VertexAttribute::new("pos", VertexFormat::Float2),
                VertexAttribute::new("uv", VertexFormat::Float2),
            ],
            shader,
        );
        let bindings = Bindings {
            vertex_buffers: vec![Buffer::immutable(ctx, BufferType::VertexBuffer, &vertices)],
            index_buffer: Buffer::immutable(ctx, BufferType::IndexBuffer, &indices),
            images: vec![
                Texture::from_rgba8(ctx, 1, 1, &[255, 0, 0, 255]),
                Texture::from_rgba8(ctx, 1, 1, &[255, 255, 255, 255]),
            ],
        };
        let target = Texture::new_render_texture(
            ctx,
            TextureParams {
                width: 32,
                height: 32,
                ..Default::default()
            },
        );
        let pass = RenderPass::new_multisampled(ctx, target, 4);
        assert_eq!(pass.samples(ctx), 4, "This regression requires MSAA=4");
        Self {
            pipeline,
            bindings,
            target,
            pass,
        }
    }

    fn prepare(&self, ctx: &mut Context) {
        ctx.begin_pass(self.pass, PassAction::clear_color(0.0, 0.0, 0.0, 1.0));
        ctx.apply_pipeline(&self.pipeline);
        ctx.apply_bindings(&self.bindings);
        ctx.apply_viewport(4, 4, 24, 24);
        ctx.apply_scissor_rect(14, 8, 10, 16);
        ctx.apply_uniforms(&[1.0f32, 0.5, 0.25, 1.0]);
    }

    fn check_pixels(&self, ctx: &mut Context, center: [u8; 4]) {
        let mut pixels = [0u8; 32 * 32 * 4];
        self.target.read_pixels(ctx, &mut pixels);
        for (x, expected) in [(16, center), (11, [0, 0, 0, 255]), (23, [0, 0, 0, 255])] {
            let pixel = &pixels[(16 * 32 + x) * 4..(16 * 32 + x + 1) * 4];
            assert!(
                pixel
                    .iter()
                    .zip(expected)
                    .all(|(&actual, expected)| actual.abs_diff(expected) <= 1),
                "pixel ({x},16): {pixel:?}, expected {expected:?}"
            );
        }
    }
}

impl EventHandler for Regression {
    fn update(&mut self, _: &mut Context) {}

    fn draw(&mut self, ctx: &mut Context) {
        self.prepare(ctx);
        ctx.draw(0, 6, 1);
        ctx.flush_draw_commands();
        // Glyph atlas growth follows this sequence: finish old draws, delete
        // their sampled texture, then upload to its replacement while the same
        // offscreen frame is still active. The old code barriers a null image.
        self.bindings.images[0].delete(ctx);
        self.bindings.images[0] = Texture::from_rgba8(ctx, 1, 1, &[0, 0, 255, 255]);
        self.bindings.images[0].update_texture_part(ctx, 0, 0, 1, 1, &[0, 255, 0, 255]);

        self.prepare(ctx);
        // Updating an already-bound texture twice must retain the target,
        // viewport, scissor, uniform slice and the other sampler's binding.
        self.bindings.images[0].update_texture_part(ctx, 0, 0, 1, 1, &[0, 0, 255, 255]);
        self.bindings.images[0].update_texture_part(ctx, 0, 0, 1, 1, &[0, 255, 0, 255]);
        ctx.draw(0, 6, 1);
        self.check_pixels(ctx, [0, 128, 0, 255]);

        // Resize keeps the public index but replaces the Vulkan view/sampler.
        // Use the normal begin-pass/bind sequence after resource synchronization.
        self.prepare(ctx);
        self.bindings.images[0].resize(ctx, 2, 2, Some(&[0, 0, 255, 255].repeat(4)));
        self.prepare(ctx);
        self.bindings.images[0].update_texture_part(ctx, 0, 0, 2, 2, &[0, 0, 255, 255].repeat(4));
        ctx.draw(0, 6, 1);
        self.check_pixels(ctx, [0, 0, 64, 255]);
        ctx.vulkan_context().unwrap().finish_offscreen_frame();
        ctx.vulkan_context().unwrap().wait_offscreen_idle();
        println!("Vulkan texture lifetime regression passed");
        ctx.quit();
    }
}

fn main() {
    let previous = std::env::var_os("PRPR_VULKAN_THREADS");
    for threads in [1, 4] {
        std::env::set_var("PRPR_VULKAN_THREADS", threads.to_string());
        let mut conf = conf::Conf {
            headless: true,
            window_width: 32,
            window_height: 32,
            ..Default::default()
        };
        conf.platform.rendering_backend = conf::RenderingBackend::Vulkan;
        miniquad::start(conf, |mut ctx| Box::new(Regression::new(&mut ctx)));
    }
    if let Some(previous) = previous {
        std::env::set_var("PRPR_VULKAN_THREADS", previous);
    } else {
        std::env::remove_var("PRPR_VULKAN_THREADS");
    }
}
