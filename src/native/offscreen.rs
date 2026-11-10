//! Shared offscreen event loop for Vulkan and Metal.
use crate::{conf::Conf, Context, EventHandler, NativeDisplay};

struct Display {
    size: (f32, f32),
    quit: bool,
}
impl NativeDisplay for Display {
    fn screen_size(&self) -> (f32, f32) {
        self.size
    }
    fn dpi_scale(&self) -> f32 {
        1.0
    }
    fn high_dpi(&self) -> bool {
        false
    }
    fn request_quit(&mut self) {
        self.quit = true;
    }
    fn order_quit(&mut self) {
        self.quit = true;
    }
    fn cancel_quit(&mut self) {
        self.quit = false;
    }
    fn set_cursor_grab(&mut self, _: bool) {}
    fn show_mouse(&mut self, _: bool) {}
    fn set_mouse_cursor(&mut self, _: crate::CursorIcon) {}
    fn set_window_size(&mut self, _: u32, _: u32) {}
    fn set_fullscreen(&mut self, _: bool) {}
    fn clipboard_get(&mut self) -> Option<String> {
        None
    }
    fn clipboard_set(&mut self, _: &str) {}
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

pub fn run<F>(conf: &Conf, create: F)
where
    F: 'static + FnOnce(&mut Context) -> Box<dyn EventHandler>,
{
    let mut display = Box::new(Display {
        size: (conf.window_width as f32, conf.window_height as f32),
        quit: false,
    });
    let mut context = Box::new(Context::new(
        conf.platform.rendering_backend,
        false,
        None,
        (conf.window_width as u32, conf.window_height as u32),
    ));
    context.display = Some(display.as_mut() as *mut dyn NativeDisplay);
    // The handler drops before the context; renderer caches must be explicitly
    // cleared while drawing, independently of platform TLS destructor ordering.
    let mut handler = create(&mut context);
    while !display.quit {
        handler.update(&mut context);
        handler.draw(&mut context);
    }
}
