//! Headless rendering backend using EGL Pbuffer.
//! No window, no X11/Wayland dependency. Runs entirely off-screen.

use crate::{
    conf::Conf,
    event::EventHandler,
    gl,
    graphics::GraphicsContext,
    native::{egl, NativeDisplayData},
    Context,
};

use std::os::raw::c_void;
use std::convert::TryInto;

/// Internal display state for headless mode.
pub struct HeadlessDisplay {
    libegl: egl::LibEgl,
    egl_display: egl::EGLDisplay,
    egl_context: egl::EGLContext,
    egl_surface: egl::EGLSurface,
    config: egl::EGLConfig,
    width: u32,
    height: u32,
    data: NativeDisplayData,
}

impl crate::native::NativeDisplay for HeadlessDisplay {
    fn screen_size(&self) -> (f32, f32) {
        (self.width as f32, self.height as f32)
    }

    fn dpi_scale(&self) -> f32 {
        1.0
    }

    fn high_dpi(&self) -> bool {
        false
    }

    fn order_quit(&mut self) {
        self.data.quit_ordered = true;
    }

    fn request_quit(&mut self) {
        self.data.quit_requested = true;
    }

    fn cancel_quit(&mut self) {
        self.data.quit_requested = false;
    }

    fn set_cursor_grab(&mut self, _grab: bool) {}
    fn show_mouse(&mut self, _shown: bool) {}
    fn set_mouse_cursor(&mut self, _cursor_icon: crate::CursorIcon) {}
    fn set_window_size(&mut self, _new_width: u32, _new_height: u32) {}
    fn set_fullscreen(&mut self, _fullscreen: bool) {}
    fn clipboard_get(&mut self) -> Option<String> {
        None
    }
    fn clipboard_set(&mut self, _data: &str) {}

    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Creates an EGL context with a Pbuffer surface for headless rendering.
unsafe fn create_pbuffer_context(
    egl: &mut egl::LibEgl,
    width: i32,
    height: i32,
    alpha: bool,
) -> Result<(egl::EGLContext, egl::EGLConfig, egl::EGLDisplay, egl::EGLSurface), String> {
    let display = (egl.eglGetDisplay.unwrap())(egl::EGL_DEFAULT_DISPLAY);  // 直接传，无需 as
    if display.is_null() {
        return Err("eglGetDisplay failed".to_string());
    }

    if (egl.eglInitialize.unwrap())(display, std::ptr::null_mut(), std::ptr::null_mut()) == 0 {
        return Err("eglInitialize failed".to_string());
    }

    let alpha_size = if alpha { 8 } else { 0 };

    // 使用 Vec<i32> 方便追加 EGL_NONE
    let cfg_attributes = vec![
        egl::EGL_SURFACE_TYPE as i32,
        egl::EGL_PBUFFER_BIT as i32,
        egl::EGL_RED_SIZE as i32, 8,
        egl::EGL_GREEN_SIZE as i32, 8,
        egl::EGL_BLUE_SIZE as i32, 8,
        egl::EGL_ALPHA_SIZE as i32, alpha_size,
        egl::EGL_DEPTH_SIZE as i32, 16,
        egl::EGL_STENCIL_SIZE as i32, 0,
        egl::EGL_NONE as i32,
    ];

    let mut configs: [egl::EGLConfig; 32] = [std::ptr::null_mut(); 32];
    let mut num_configs = 0;
    if (egl.eglChooseConfig.unwrap())(
        display,
        cfg_attributes.as_ptr(),
        configs.as_mut_ptr(),
        configs.len() as i32,
        &mut num_configs,
    ) == 0 {
        return Err("eglChooseConfig failed".to_string());
    }
    if num_configs == 0 {
        return Err("No suitable EGL config found".to_string());
    }

    let config = configs[0];

    // Pbuffer 属性也使用 i32
    let pbuffer_attrs = [
        egl::EGL_WIDTH as i32,
        width,
        egl::EGL_HEIGHT as i32,
        height,
        egl::EGL_NONE as i32,
    ];
    let surface = (egl.eglCreatePbufferSurface.unwrap())(
        display,
        config,
        pbuffer_attrs.as_ptr(),
    );
    if surface.is_null() {
        return Err("eglCreatePbufferSurface failed".to_string());
    }

    // Context 属性
    let ctx_attrs = [
        egl::EGL_CONTEXT_CLIENT_VERSION as i32,
        2,
        egl::EGL_NONE as i32,
    ];
    let context = (egl.eglCreateContext.unwrap())(
        display,
        config,
        std::ptr::null_mut(),
        ctx_attrs.as_ptr(),
    );
    if context.is_null() {
        return Err("eglCreateContext failed".to_string());
    }

    if (egl.eglMakeCurrent.unwrap())(display, surface, surface, context) == 0 {
        return Err("eglMakeCurrent failed".to_string());
    }

    Ok((context, config, display, surface))
}

/// Main entry point for headless rendering.
pub fn run<F>(conf: &Conf, f: &mut Option<F>) -> Option<()>
where
    F: 'static + FnOnce(&mut Context) -> Box<dyn EventHandler>,
{
    unsafe {
        // Load EGL library
        let mut libegl = egl::LibEgl::try_load()?;

        // Create Pbuffer context
        let (context, config, egl_display, surface) = match create_pbuffer_context(
            &mut libegl,
            conf.window_width as i32,
            conf.window_height as i32,
            conf.platform.framebuffer_alpha,
        ) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Headless EGL initialization error: {}", e);
                return None;
            }
        };

        // Load OpenGL functions
        gl::load_gl_funcs(|proc| {
            let name = std::ffi::CString::new(proc).unwrap();
            libegl
                .eglGetProcAddress
                .expect("eglGetProcAddress missing")(name.as_ptr() as _)
        });

        // Create GraphicsContext (will detect GL version)
        let mut graphics_ctx = GraphicsContext::new(gl::is_gl2());

        // Build display state
        let mut display = HeadlessDisplay {
            libegl,
            egl_display,
            egl_context: context,
            egl_surface: surface,
            config,
            width: conf.window_width as u32,   // 直接赋值
            height: conf.window_height as u32,
            data: NativeDisplayData::default(),
        };        // Pass display to user's event handler
        let mut context = graphics_ctx.with_display(&mut display);
        let mut event_handler = (f.take().unwrap())(&mut context);
        unsafe {
            let renderer = std::ffi::CStr::from_ptr(gl::glGetString(gl::GL_RENDERER) as *const _)
                .to_str()
                .unwrap();
            eprintln!("OpenGL Renderer: {}", renderer);
        }

        // Main loop
        while !display.data.quit_ordered {
            // Process any pending events (none in headless, but we still call update/draw)
            event_handler.update(&mut context);
            event_handler.draw(&mut context);

            // Swap buffers (for Pbuffer this may be a no-op, but some EGL implementations need it)
            (display.libegl.eglSwapBuffers.unwrap())(display.egl_display, display.egl_surface);

            // Check for quit request from event handler
            if display.data.quit_requested && !display.data.quit_ordered {
                event_handler.quit_requested_event(&mut context);
                if display.data.quit_requested {
                    display.data.quit_ordered = true;
                }
            }
        }

        // Cleanup
        (display.libegl.eglMakeCurrent.unwrap())(
            display.egl_display,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        (display.libegl.eglDestroySurface.unwrap())(display.egl_display, display.egl_surface);
        (display.libegl.eglDestroyContext.unwrap())(display.egl_display, display.egl_context);
        (display.libegl.eglTerminate.unwrap())(display.egl_display);

        Some(())
    }
}
