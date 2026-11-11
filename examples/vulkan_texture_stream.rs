//! Real Vulkan regression for uploads interleaved with rendering.
//!
//! On Linux (no display server required):
//! `cargo run --example vulkan_texture_stream --features vulkan,headless`
//! Runs both the direct and multithreaded command recorders and exits on success.
#[cfg(all(feature = "vulkan", target_os = "linux"))]
mod smoke {
    use ash::vk;
    use miniquad::*;

    const WIDTH: u32 = 64;
    const HEIGHT: u32 = 32;
    const FRAMES: usize = 6; // Reuse both fence-protected slots three times.
    const LARGE_WIDTH: u32 = 1024;
    const LARGE_HEIGHT: u32 = 1025; // One upload exceeds the initial 4 MiB arena.
    const WHITE: [f32; 4] = [1.; 4];

    #[derive(Clone, Copy)]
    struct Target {
        texture: Texture,
        pass: RenderPass,
    }

    impl Target {
        fn new(ctx: &mut Context) -> Self {
            let texture = Texture::new_render_texture(
                ctx,
                TextureParams {
                    width: WIDTH,
                    height: HEIGHT,
                    filter: FilterMode::Nearest,
                    ..Default::default()
                },
            );
            Self {
                texture,
                pass: RenderPass::new(ctx, texture, None),
            }
        }

        fn multisampled(ctx: &mut Context) -> Self {
            let texture = Texture::new_render_texture(
                ctx,
                TextureParams {
                    width: WIDTH,
                    height: HEIGHT,
                    filter: FilterMode::Nearest,
                    ..Default::default()
                },
            );
            let pass = RenderPass::new_multisampled(ctx, texture, 4);
            assert_eq!(pass.samples(ctx), 4, "This regression requires MSAA=4");
            Self { texture, pass }
        }
    }

    fn check_image(
        ctx: &mut Context,
        target: Target,
        label: &str,
        frame: usize,
        expected: impl Fn(u32, u32) -> [u8; 4],
    ) {
        let mut bytes = vec![0; (WIDTH * HEIGHT * 4) as usize];
        target.texture.read_pixels(ctx, &mut bytes);
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let i = ((y * WIDTH + x) * 4) as usize;
                assert_eq!(
                    &bytes[i..i + 4],
                    &expected(x, y),
                    "{label}, frame {frame}, pixel ({x}, {y})"
                );
            }
        }
    }

    fn check_rgb_upload_offset(ctx: &mut Context, slot: usize, bytes: u64) {
        let arena = &ctx.vulkan_context().unwrap().stream_arenas[slot];
        let offset = arena.offset - bytes;
        assert!(offset > 0, "RGB upload must exercise a nonzero arena offset");
        assert_eq!(offset % 3, 0, "RGB copy offset must align to its texel block");
        assert_eq!(offset % 16, 0, "preserve the stream arena's base alignment");
    }

    struct Smoke {
        threads: usize,
    }

    impl EventHandler for Smoke {
        fn update(&mut self, _: &mut Context) {}

        fn draw(&mut self, ctx: &mut Context) {
            let source = Texture::new(
                ctx,
                TextureAccess::Static,
                Some(&[0; 16]),
                TextureParams {
                    width: 2,
                    height: 2,
                    filter: FilterMode::Nearest,
                    ..Default::default()
                },
            );
            let large = Texture::new(
                ctx,
                TextureAccess::Static,
                None,
                TextureParams {
                    width: LARGE_WIDTH,
                    height: LARGE_HEIGHT,
                    filter: FilterMode::Nearest,
                    ..Default::default()
                },
            );
            // RGB render textures use RGBA Vulkan storage: updates must expand
            // RGB bytes and sampling must still return alpha=1.
            let rgb = Texture::new_render_texture(
                ctx,
                TextureParams {
                    width: 2,
                    height: 2,
                    format: TextureFormat::RGB8,
                    filter: FilterMode::Nearest,
                    ..Default::default()
                },
            );
            // Three-channel sampled images are optional in Vulkan. Test true
            // RGB storage when supported, rather than the RGBA-backed target.
            let static_rgb_supported = {
                let vk_ctx = ctx.vulkan_context().unwrap();
                unsafe {
                    vk_ctx.instance.get_physical_device_image_format_properties(
                        vk_ctx.physical_device,
                        vk::Format::R8G8B8_UNORM,
                        vk::ImageType::TYPE_2D,
                        vk::ImageTiling::OPTIMAL,
                        vk::ImageUsageFlags::SAMPLED
                            | vk::ImageUsageFlags::TRANSFER_SRC
                            | vk::ImageUsageFlags::TRANSFER_DST,
                        vk::ImageCreateFlags::empty(),
                    ).is_ok()
                }
            };
            let static_rgb = if static_rgb_supported {
                Some(Texture::new(
                    ctx,
                    TextureAccess::Static,
                    Some(&[0; 12]),
                    TextureParams {
                        width: 2,
                        height: 2,
                        format: TextureFormat::RGB8,
                        filter: FilterMode::Nearest,
                        ..Default::default()
                    },
                ))
            } else {
                println!("SKIP static RGB8 uploads: device does not support sampled/transfer R8G8B8_UNORM; pure stream_alignment_tests still cover offset alignment");
                None
            };
            let mut resized_source = Texture::from_rgba8(ctx, 2, 2, &[255, 0, 0, 255].repeat(4));
            let shader = Shader::new(
                ctx,
                r#"#version 450 core
layout(location=0) in vec2 position;
layout(location=0) out vec2 uv;
void main() { gl_Position=vec4(position,0,1); uv=(position+1)*0.5; }"#,
                r#"#version 450 core
layout(location=0) in vec2 uv;
layout(location=0) out vec4 color;
layout(set=0,binding=0) uniform Uniforms { vec4 tint; };
layout(set=0,binding=1) uniform sampler2D image;
void main() { color=texture(image,uv)*tint; }"#,
                ShaderMeta {
                    images: vec!["image".into()],
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
            let vertices = Buffer::immutable(
                ctx,
                BufferType::VertexBuffer,
                &[[-1f32, -1.], [3., -1.], [-1., 3.]],
            );
            let indices = Buffer::immutable(ctx, BufferType::IndexBuffer, &[0u16, 1, 2]);
            let bindings = |texture| Bindings {
                vertex_buffers: vec![vertices],
                index_buffer: indices,
                images: vec![texture],
            };
            // Allocate before recording so resource creation cannot accidentally
            // serialize the frames this test is meant to keep in flight.
            let targets: Vec<[Target; 5]> = (0..FRAMES)
                .map(|_| std::array::from_fn(|_| Target::new(ctx)))
                .collect();
            let msaa_targets: Vec<Target> =
                (0..FRAMES).map(|_| Target::multisampled(ctx)).collect();
            let immediate = Target::new(ctx);
            let resize_target = Target::new(ctx);
            ctx.vulkan_context().unwrap().enable_offscreen_video();

            for (frame, target) in targets.iter().enumerate() {
                let f = frame as u8;
                let red = [255, 0, f + 1, 255];
                let green = [0, 255, f + 1, 255];
                let large_color = [11 + f, 61 + f, 191 - f, 255];
                let slot = ctx.vulkan_context().unwrap().begin_offscreen_frame();
                assert_eq!(slot, frame % 2);

                ctx.begin_pass(target[0].pass, PassAction::clear_color(0., 0., 0., 1.));
                // Updating with a pass already open must preserve that pass.
                source.update(ctx, &red.repeat(4));
                ctx.apply_pipeline(&pipeline);
                ctx.apply_bindings(&bindings(source));
                ctx.apply_scissor_rect(0, 0, WIDTH as i32 / 2, HEIGHT as i32);
                for _ in 0..64 {
                    ctx.apply_uniforms(&WHITE);
                    ctx.draw(0, 3, 1);
                }

                // Growing the arena must keep the earlier red upload alive.
                // Its draw batch is large enough to exercise secondary command
                // buffers with PRPR_VULKAN_THREADS > 1.
                large.update(
                    ctx,
                    &large_color.repeat((LARGE_WIDTH * LARGE_HEIGHT) as usize),
                );
                ctx.apply_scissor_rect(WIDTH as i32 / 2, 0, WIDTH as i32 / 2, HEIGHT as i32);
                ctx.apply_uniforms(&WHITE);
                source.update(ctx, &green.repeat(4));
                // Deliberately do not rebind the pass, pipeline, descriptors,
                // viewport, scissor, or uniforms after the update.
                ctx.draw(0, 3, 1);
                for _ in 1..64 {
                    ctx.apply_uniforms(&WHITE);
                    ctx.draw(0, 3, 1);
                }
                ctx.end_render_pass();

                ctx.begin_pass(target[1].pass, PassAction::clear_color(0., 0., 0., 1.));
                source.update(ctx, &[0, 0, 0, 255].repeat(4));
                source.update_texture_part(ctx, 1, 0, 1, 2, &[f, 0, 255, 255].repeat(2));
                ctx.apply_pipeline(&pipeline);
                ctx.apply_bindings(&bindings(source));
                ctx.apply_uniforms(&WHITE);
                ctx.draw(0, 3, 1);
                ctx.end_render_pass();

                ctx.begin_pass(target[2].pass, PassAction::clear_color(0., 0., 0., 1.));
                ctx.apply_pipeline(&pipeline);
                ctx.apply_bindings(&bindings(large));
                ctx.apply_uniforms(&WHITE);
                ctx.draw(0, 3, 1);
                ctx.end_render_pass();

                ctx.begin_pass(target[3].pass, PassAction::clear_color(0., 0., 0., 1.));
                rgb.update(ctx, &[17 + f, 31, 47].repeat(4));
                rgb.update_texture_part(ctx, 1, 0, 1, 2, &[91 + f, 127, 213].repeat(2));
                ctx.apply_pipeline(&pipeline);
                ctx.apply_bindings(&bindings(rgb));
                ctx.apply_uniforms(&WHITE);
                ctx.draw(0, 3, 1);
                ctx.end_render_pass();

                if let Some(static_rgb) = static_rgb {
                    ctx.begin_pass(target[4].pass, PassAction::clear_color(0., 0., 0., 1.));
                    // Consecutive small uploads force the old 16-byte allocator
                    // to produce an offset not divisible by a three-byte texel.
                    static_rgb.update(ctx, &[1, 2, 3].repeat(4));
                    check_rgb_upload_offset(ctx, slot, 12);
                    static_rgb.update(ctx, &[21 + f, 51, 81].repeat(4));
                    check_rgb_upload_offset(ctx, slot, 12);
                    static_rgb.update_texture_part(ctx, 1, 0, 1, 2, &[151 + f, 181, 211].repeat(2));
                    check_rgb_upload_offset(ctx, slot, 6);
                    ctx.apply_pipeline(&pipeline);
                    ctx.apply_bindings(&bindings(static_rgb));
                    ctx.apply_uniforms(&WHITE);
                    ctx.draw(0, 3, 1);
                    ctx.end_render_pass();
                }

                // The first update ends and resolves the MSAA scope. Resuming
                // must LOAD the previous four samples for the untouched half,
                // then resolve both halves after the later green draw.
                ctx.begin_pass(
                    msaa_targets[frame].pass,
                    PassAction::clear_color(0., 0., 0., 1.),
                );
                source.update(ctx, &red.repeat(4));
                ctx.apply_pipeline(&pipeline);
                ctx.apply_bindings(&bindings(source));
                ctx.apply_scissor_rect(0, 0, WIDTH as i32 / 2, HEIGHT as i32);
                for _ in 0..64 {
                    ctx.apply_uniforms(&WHITE);
                    ctx.draw(0, 3, 1);
                }
                ctx.apply_scissor_rect(WIDTH as i32 / 2, 0, WIDTH as i32 / 2, HEIGHT as i32);
                ctx.apply_uniforms(&WHITE);
                source.update(ctx, &green.repeat(4));
                ctx.draw(0, 3, 1);
                for _ in 1..64 {
                    ctx.apply_uniforms(&WHITE);
                    ctx.draw(0, 3, 1);
                }
                ctx.end_render_pass();
                ctx.vulkan_context().unwrap().finish_offscreen_frame();
            }

            // The six frames are submitted before the first readback. Each has
            // its own output, while all reuse and update the same source images.
            ctx.vulkan_context().unwrap().wait_offscreen_idle();
            for (frame, target) in targets.iter().enumerate() {
                let f = frame as u8;
                check_image(ctx, target[0], "draw/update/draw", frame, |x, _| {
                    if x < WIDTH / 2 {
                        [255, 0, f + 1, 255]
                    } else {
                        [0, 255, f + 1, 255]
                    }
                });
                check_image(ctx, target[1], "partial upload", frame, |x, _| {
                    if x < WIDTH / 2 {
                        [0, 0, 0, 255]
                    } else {
                        [f, 0, 255, 255]
                    }
                });
                check_image(ctx, target[2], "arena growth", frame, |_, _| {
                    [11 + f, 61 + f, 191 - f, 255]
                });
                check_image(ctx, target[3], "RGB expansion", frame, |x, _| {
                    if x < WIDTH / 2 {
                        [17 + f, 31, 47, 255]
                    } else {
                        [91 + f, 127, 213, 255]
                    }
                });
                if static_rgb.is_some() {
                    check_image(ctx, target[4], "static RGB aligned uploads", frame, |x, _| {
                        if x < WIDTH / 2 {
                            [21 + f, 51, 81, 255]
                        } else {
                            [151 + f, 181, 211, 255]
                        }
                    });
                }
                check_image(
                    ctx,
                    msaa_targets[frame],
                    "MSAA LOAD/resolve",
                    frame,
                    |x, _| {
                        if x < WIDTH / 2 {
                            [255, 0, f + 1, 255]
                        } else {
                            [0, 255, f + 1, 255]
                        }
                    },
                );
            }

            // A synchronous readback must flush a not-yet-submitted upload and
            // leave the frame usable for later rendering and submission.
            ctx.vulkan_context().unwrap().begin_offscreen_frame();
            ctx.begin_pass(immediate.pass, PassAction::clear_color(0., 0., 0., 1.));
            let amber = [237, 149, 23, 255];
            source.update(ctx, &amber.repeat(4));
            let mut pixels = [0; 16];
            source.read_pixels(ctx, &mut pixels);
            assert_eq!(&pixels[..], &amber.repeat(4));
            ctx.begin_pass(immediate.pass, PassAction::Nothing);
            ctx.apply_pipeline(&pipeline);
            ctx.apply_bindings(&bindings(source));
            ctx.apply_uniforms(&WHITE);
            ctx.draw(0, 3, 1);
            ctx.end_render_pass();
            check_image(ctx, immediate, "upload/readback/draw", FRAMES, |_, _| amber);
            ctx.vulkan_context().unwrap().finish_offscreen_frame();
            ctx.vulkan_context().unwrap().wait_offscreen_idle();

            // Resizing replaces and destroys the old image. The previous draw
            // must finish before that destruction, even though initializing
            // the replacement now uses the asynchronous upload path.
            ctx.vulkan_context().unwrap().begin_offscreen_frame();
            ctx.begin_pass(resize_target.pass, PassAction::clear_color(0., 0., 0., 1.));
            ctx.apply_pipeline(&pipeline);
            ctx.apply_bindings(&bindings(resized_source));
            ctx.apply_scissor_rect(0, 0, WIDTH as i32 / 2, HEIGHT as i32);
            for _ in 0..64 {
                ctx.apply_uniforms(&WHITE);
                ctx.draw(0, 3, 1);
            }
            resized_source.resize(ctx, 4, 3, Some(&[0, 255, 0, 255].repeat(12)));
            assert_eq!((resized_source.width, resized_source.height), (4, 3));
            // Resource replacement may flush the command buffer; restore the
            // drawing state explicitly and bind the replacement texture.
            ctx.begin_pass(resize_target.pass, PassAction::Nothing);
            ctx.apply_pipeline(&pipeline);
            ctx.apply_bindings(&bindings(resized_source));
            ctx.apply_scissor_rect(WIDTH as i32 / 2, 0, WIDTH as i32 / 2, HEIGHT as i32);
            for _ in 0..64 {
                ctx.apply_uniforms(&WHITE);
                ctx.draw(0, 3, 1);
            }
            ctx.end_render_pass();
            ctx.vulkan_context().unwrap().finish_offscreen_frame();
            ctx.vulkan_context().unwrap().wait_offscreen_idle();
            check_image(
                ctx,
                resize_target,
                "draw/resize/draw",
                FRAMES + 1,
                |x, _| {
                    if x < WIDTH / 2 {
                        [255, 0, 0, 255]
                    } else {
                        [0, 255, 0, 255]
                    }
                },
            );

            let stats = ctx.vulkan_context().unwrap().command_recording_stats();
            if self.threads > 1 {
                assert!(stats.worker_draw_calls > 0, "MT recorder was not exercised");
            }
            println!(
                "Vulkan texture streaming passed: threads={}, frames={}, worker_draws={}, same-frame snapshots, partial/RGB uploads, arena growth, MSAA LOAD/resolve, readback, resize",
                self.threads, FRAMES, stats.worker_draw_calls
            );
            ctx.request_quit();
        }
    }

    pub fn run() {
        let previous = std::env::var_os("PRPR_VULKAN_THREADS");
        for threads in [1, 4] {
            std::env::set_var("PRPR_VULKAN_THREADS", threads.to_string());
            start(
                conf::Conf {
                    headless: true,
                    window_width: WIDTH as i32,
                    window_height: HEIGHT as i32,
                    platform: conf::Platform {
                        rendering_backend: conf::RenderingBackend::Vulkan,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                move |_| Box::new(Smoke { threads }),
            );
        }
        if let Some(previous) = previous {
            std::env::set_var("PRPR_VULKAN_THREADS", previous);
        } else {
            std::env::remove_var("PRPR_VULKAN_THREADS");
        }
    }
}

#[cfg(all(feature = "vulkan", target_os = "linux"))]
fn main() {
    smoke::run();
}

#[cfg(not(all(feature = "vulkan", target_os = "linux")))]
fn main() {
    eprintln!("Run on Linux with --features vulkan,headless");
}
