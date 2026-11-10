#![allow(dead_code)]

mod libwayland_client;
mod libwayland_egl;

mod decorations;
mod extensions;
mod shm;

use libwayland_client::*;
use libwayland_egl::*;

use crate::{
    event::EventHandler,
    native::{egl, NativeDisplayData},
    Context,
};

pub(crate) struct WaylandDisplay {
    client: LibWaylandClient,
    // this is libwayland-egl.so, a library with ~4 functions
    // not the libEGL.so(which will be loaded, but not here)
    egl: Option<LibWaylandEgl>,
    compositor: *mut wl_compositor,
    subcompositor: *mut wl_subcompositor,
    xdg_toplevel: *mut extensions::xdg_shell::xdg_toplevel,
    xdg_wm_base: *mut extensions::xdg_shell::xdg_wm_base,
    surface: *mut wl_surface,
    decoration_manager: *mut extensions::xdg_decoration::zxdg_decoration_manager_v1,
    viewporter: *mut extensions::viewporter::wp_viewporter,
    shm: *mut wl_shm,
    seat: *mut wl_seat,

    egl_window: *mut wl_egl_window,
    pointer: *mut wl_pointer,
    keyboard: *mut wl_keyboard,
    focused_window: *mut wl_surface,
    //xkb_state: xkb::XkbState,
    decorations: Option<decorations::Decorations>,
    closed: bool,

    data: NativeDisplayData,
}

impl crate::native::NativeDisplay for WaylandDisplay {
    fn screen_size(&self) -> (f32, f32) {
        (self.data.screen_width as _, self.data.screen_height as _)
    }
    fn dpi_scale(&self) -> f32 {
        self.data.dpi_scale
    }
    fn high_dpi(&self) -> bool {
        self.data.high_dpi
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

/// A thing to pass around within *void pointer of wayland's event handler
struct WaylandPayload {
    display: WaylandDisplay,
    context: Option<Context>,
    event_handler: Option<Box<dyn EventHandler>>,
}

impl WaylandPayload {
    pub fn context(&mut self) -> (&mut Context, &mut Option<Box<dyn EventHandler>>) {
        (
            self.context
                .as_mut()
                .unwrap()
                .with_display(&mut self.display),
            &mut self.event_handler,
        )
    }
}

#[macro_export]
macro_rules! wl_request_constructor {
    ($libwayland:expr, $instance:expr, $request_name:expr, $interface:expr) => {
        wl_request_constructor!($libwayland, $instance, $request_name, $interface, ())
    };

    ($libwayland:expr, $instance:expr, $request_name:expr, $interface:expr, $($arg:expr),*) => {{
        let id: *mut wl_proxy;

        id = ($libwayland.wl_proxy_marshal_constructor)(
            $instance as _,
            $request_name,
            $interface as _,
            std::ptr::null_mut::<std::ffi::c_void>(),
            $($arg,)*
        );

        id as *mut _
    }};
}

#[macro_export]
macro_rules! wl_request {
    ($libwayland:expr, $instance:expr, $request_name:expr) => {
        wl_request!($libwayland, $instance, $request_name, ())
    };

    ($libwayland:expr, $instance:expr, $request_name:expr, $($arg:expr),*) => {{
        ($libwayland.wl_proxy_marshal)(
            $instance as _,
            $request_name,
            $($arg,)*
        )
    }};
}

static mut SEAT_LISTENER: wl_seat_listener = wl_seat_listener {
    capabilities: Some(seat_handle_capabilities),
    name: Some(seat_handle_name),
};

unsafe extern "C" fn seat_handle_capabilities(
    _data: *mut std::ffi::c_void,
    _seat: *mut wl_seat,
    _caps: wl_seat_capability,
) {
}

extern "C" fn seat_handle_name(
    _data: *mut std::ffi::c_void,
    _seat: *mut wl_seat,
    _name: *const ::std::os::raw::c_char,
) {
}

unsafe extern "C" fn registry_add_object(
    data: *mut std::ffi::c_void,
    registry: *mut wl_registry,
    name: u32,
    interface: *const ::std::os::raw::c_char,
    version: u32,
) {
    let payload: &mut WaylandPayload = &mut *(data as *mut _);
    let display = &mut payload.display;

    let interface = std::ffi::CStr::from_ptr(interface).to_str().unwrap();
    crate::diagnostic!(3, "{:?}", interface);
    match interface {
        "wl_compositor" => {
            display.compositor = display.client.wl_registry_bind(
                registry,
                name,
                display.client.wl_compositor_interface,
                1,
            ) as _;
        }
        "wl_subcompositor" => {
            display.subcompositor = display.client.wl_registry_bind(
                registry,
                name,
                display.client.wl_subcompositor_interface,
                1,
            ) as _;
        }
        "xdg_wm_base" => {
            display.xdg_wm_base = display.client.wl_registry_bind(
                registry,
                name,
                &extensions::xdg_shell::xdg_wm_base_interface,
                1,
            ) as _;
        }
        "zxdg_decoration_manager" => {
            display.decoration_manager = display.client.wl_registry_bind(
                registry,
                name,
                &extensions::xdg_decoration::zxdg_decoration_manager_v1_interface,
                1,
            ) as _;
        }
        "wp_viewporter" => {
            display.viewporter = display.client.wl_registry_bind(
                registry,
                name,
                &extensions::viewporter::wp_viewporter_interface,
                1,
            ) as _;
        }
        "wl_shm" => {
            display.shm =
                display
                    .client
                    .wl_registry_bind(registry, name, display.client.wl_shm_interface, 1)
                    as _;
        }
        "wl_seat" => {
            let seat_version = 4.min(version);
            display.seat = display.client.wl_registry_bind(
                registry,
                name,
                display.client.wl_seat_interface,
                seat_version,
            ) as _;
            (display.client.wl_proxy_add_listener)(
                display.seat as _,
                &SEAT_LISTENER as *const _ as _,
                data,
            );
        }

        _ => {}
    }
}

unsafe extern "C" fn registry_remove_object(
    _data: *mut std::ffi::c_void,
    _registry: *mut wl_registry,
    _name: u32,
) {
}

unsafe extern "C" fn xdg_surface_handle_configure(
    data: *mut std::ffi::c_void,
    xdg_surface: *mut extensions::xdg_shell::xdg_surface,
    serial: u32,
) {
    assert!(!data.is_null());
    let payload: &mut WaylandPayload = &mut *(data as *mut _);

    wl_request!(
        payload.display.client,
        xdg_surface,
        extensions::xdg_shell::xdg_surface::ack_configure,
        serial
    );
    wl_request!(
        payload.display.client,
        payload.display.surface,
        WL_SURFACE_COMMIT
    );
}

unsafe extern "C" fn xdg_toplevel_handle_close(
    data: *mut std::ffi::c_void,
    _xdg_toplevel: *mut extensions::xdg_shell::xdg_toplevel,
) {
    assert!(!data.is_null());
    let mut payload: &mut WaylandPayload = &mut *(data as *mut _);

    payload.display.closed = true;
}

unsafe extern "C" fn xdg_toplevel_handle_configure(
    data: *mut std::ffi::c_void,
    _toplevel: *mut extensions::xdg_shell::xdg_toplevel,
    width: i32,
    height: i32,
    _states: *mut wl_array,
) -> () {
    assert!(!data.is_null());
    let payload: &mut WaylandPayload = &mut *(data as *mut _);
    let display = &mut payload.display;

    if width != 0 && height != 0 {
        let (egl_w, egl_h) = if display.decorations.is_some() {
            // Otherwise window will resize iteself on sway
            // I have no idea why
            (
                width - decorations::Decorations::WIDTH * 2,
                height - decorations::Decorations::BAR_HEIGHT - decorations::Decorations::WIDTH,
            )
        } else {
            (width, height)
        };
        let egl_w = egl_w.max(1);
        let egl_h = egl_h.max(1);
        if let Some(egl) = &display.egl {
            if !display.egl_window.is_null() {
                (egl.wl_egl_window_resize)(display.egl_window, egl_w, egl_h, 0, 0);
            }
        }

        display.data.screen_width = width;
        display.data.screen_height = height;

        if let Some(ref decorations) = display.decorations {
            decorations.resize(&mut display.client, width, height);
        }

        if let Some(context) = &mut payload.context {
            context.resize(egl_w as u32, egl_h as u32);
            if let Some(event_handler) = &mut payload.event_handler {
                event_handler.resize_event(
                    context.with_display(&mut payload.display),
                    width as _,
                    height as _,
                );
            }
        }
    }
}

pub fn run<F>(conf: &crate::conf::Conf, f: &mut Option<F>) -> Option<()>
where
    F: 'static + FnOnce(&mut Context) -> Box<dyn EventHandler>,
{
    unsafe {
        let vulkan = conf.platform.rendering_backend == crate::conf::RenderingBackend::Vulkan;
        let client = LibWaylandClient::try_load()?;
        let egl = if vulkan {
            None
        } else {
            Some(LibWaylandEgl::try_load()?)
        };

        let wdisplay = (client.wl_display_connect)(std::ptr::null_mut());
        if wdisplay.is_null() {
            crate::diagnostic!(3, "Failed to connect to Wayland payload.display.");
            return None;
        }

        let registry: *mut wl_proxy = wl_request_constructor!(
            client,
            wdisplay,
            WL_DISPLAY_GET_REGISTRY,
            client.wl_registry_interface
        );
        assert!(!registry.is_null());

        let registry_listener = wl_registry_listener {
            global: Some(registry_add_object),
            global_remove: Some(registry_remove_object),
        };

        let display = WaylandDisplay {
            client,
            egl,
            compositor: std::ptr::null_mut(),
            subcompositor: std::ptr::null_mut(),
            xdg_toplevel: std::ptr::null_mut(),
            xdg_wm_base: std::ptr::null_mut(),
            surface: std::ptr::null_mut(),
            decoration_manager: std::ptr::null_mut(),
            viewporter: std::ptr::null_mut(),
            shm: std::ptr::null_mut(),
            seat: std::ptr::null_mut(),
            egl_window: std::ptr::null_mut(),
            pointer: std::ptr::null_mut(),
            keyboard: std::ptr::null_mut(),
            focused_window: std::ptr::null_mut(),
            //xkb_state: xkb::XkbState::new(),
            decorations: None,
            closed: false,
            data: Default::default(),
        };
        let mut payload = WaylandPayload {
            display,
            context: None,
            event_handler: None,
        };
        (payload.display.client.wl_proxy_add_listener)(
            registry,
            &registry_listener as *const _ as _,
            &mut payload as *mut _ as _,
        );
        (payload.display.client.wl_display_roundtrip)(wdisplay);

        assert!(payload.display.compositor.is_null() == false);
        assert!(payload.display.xdg_wm_base.is_null() == false);
        assert!(payload.display.subcompositor.is_null() == false);
        assert!(payload.display.seat.is_null() == false);

        if payload.display.decoration_manager.is_null() {
            crate::diagnostic!(3, "Decoration manager not found, will draw fallback decorations");
        }

        payload.display.surface = wl_request_constructor!(
            payload.display.client,
            payload.display.compositor,
            WL_COMPOSITOR_CREATE_SURFACE,
            payload.display.client.wl_surface_interface
        );
        assert!(payload.display.surface.is_null() == false);

        let xdg_surface: *mut extensions::xdg_shell::xdg_surface = wl_request_constructor!(
            payload.display.client,
            payload.display.xdg_wm_base,
            extensions::xdg_shell::xdg_wm_base::get_xdg_surface,
            &extensions::xdg_shell::xdg_surface_interface,
            payload.display.surface
        );
        assert!(xdg_surface.is_null() == false);

        let xdg_surface_listener = extensions::xdg_shell::xdg_surface_listener {
            configure: Some(xdg_surface_handle_configure),
        };

        (payload.display.client.wl_proxy_add_listener)(
            xdg_surface as _,
            &xdg_surface_listener as *const _ as _,
            &mut payload as *mut _ as _,
        );

        payload.display.xdg_toplevel = wl_request_constructor!(
            payload.display.client,
            xdg_surface,
            extensions::xdg_shell::xdg_surface::get_toplevel,
            &extensions::xdg_shell::xdg_toplevel_interface
        );
        assert!(payload.display.xdg_toplevel.is_null() == false);

        let xdg_toplevel_listener = extensions::xdg_shell::xdg_toplevel_listener {
            configure: Some(xdg_toplevel_handle_configure),
            close: Some(xdg_toplevel_handle_close),
        };

        (payload.display.client.wl_proxy_add_listener)(
            payload.display.xdg_toplevel as _,
            &xdg_toplevel_listener as *const _ as _,
            &mut payload as *mut _ as _,
        );

        wl_request!(
            payload.display.client,
            payload.display.surface,
            WL_SURFACE_COMMIT
        );
        (payload.display.client.wl_display_roundtrip)(wdisplay);

        if payload.display.decoration_manager.is_null() == false {
            let server_decoration: *mut extensions::xdg_decoration::zxdg_toplevel_decoration_v1 = wl_request_constructor!(
                payload.display.client,
                payload.display.decoration_manager,
                extensions::xdg_decoration::zxdg_decoration_manager_v1::get_toplevel_decoration,
                &extensions::xdg_decoration::zxdg_toplevel_decoration_v1_interface,
                payload.display.xdg_toplevel
            );
            assert!(server_decoration.is_null() == false);

            wl_request!(
                payload.display.client,
                server_decoration,
                extensions::xdg_decoration::zxdg_toplevel_decoration_v1::set_mode,
                extensions::xdg_decoration::ZXDG_TOPLEVEL_DECORATION_V1_MODE_SERVER_SIDE
            );
        } else {
            payload.display.decorations = Some(decorations::Decorations::new(
                &mut payload.display,
                conf.window_width,
                conf.window_height,
            ));
        }

        if payload.display.data.screen_width <= 0 || payload.display.data.screen_height <= 0 {
            payload.display.data.screen_width = conf.window_width;
            payload.display.data.screen_height = conf.window_height;
        }
        let (drawable_width, drawable_height) = if payload.display.decorations.is_some() {
            (
                payload.display.data.screen_width - decorations::Decorations::WIDTH * 2,
                payload.display.data.screen_height
                    - decorations::Decorations::BAR_HEIGHT
                    - decorations::Decorations::WIDTH,
            )
        } else {
            (
                payload.display.data.screen_width,
                payload.display.data.screen_height,
            )
        };
        let drawable_width = drawable_width.max(1) as u32;
        let drawable_height = drawable_height.max(1) as u32;

        let mut opengl = if vulkan {
            None
        } else {
            let mut libegl = egl::LibEgl::try_load()?;
            let (egl_context, config, egl_display) = egl::create_egl_context(
                &mut libegl,
                wdisplay as *mut _,
                conf.platform.framebuffer_alpha,
            )
            .unwrap();
            let wayland_egl = payload.display.egl.as_ref().unwrap();
            payload.display.egl_window = (wayland_egl.wl_egl_window_create)(
                payload.display.surface as _,
                drawable_width as _,
                drawable_height as _,
            );
            let egl_surface = (libegl.eglCreateWindowSurface.unwrap())(
                egl_display,
                config,
                payload.display.egl_window as _,
                std::ptr::null_mut(),
            );
            if egl_surface.is_null() {
                panic!("surface creation failed");
            }
            if (libegl.eglMakeCurrent.unwrap())(egl_display, egl_surface, egl_surface, egl_context)
                == 0
            {
                panic!("eglMakeCurrent failed");
            }
            crate::native::gl::load_gl_funcs(|proc| {
                let name = std::ffi::CString::new(proc).unwrap();
                libegl.eglGetProcAddress.expect("non-null function pointer")(name.as_ptr() as _)
            });
            Some((libegl, egl_display, egl_surface))
        };

        payload.context = Some(if vulkan {
            let mut display_handle = raw_window_handle::WaylandDisplayHandle::empty();
            display_handle.display = wdisplay as *mut _;
            let mut window_handle = raw_window_handle::WaylandWindowHandle::empty();
            window_handle.surface = payload.display.surface as *mut _;
            Context::new(
                crate::conf::RenderingBackend::Vulkan,
                false,
                Some((
                    raw_window_handle::RawDisplayHandle::Wayland(display_handle),
                    raw_window_handle::RawWindowHandle::Wayland(window_handle),
                )),
                (drawable_width, drawable_height),
            )
        } else {
            Context::new(
                crate::conf::RenderingBackend::OpenGL,
                false,
                None,
                (drawable_width, drawable_height),
            )
        });

        let event_handler = (f.take().unwrap())(&mut payload.context().0);
        payload.event_handler = Some(event_handler);

        while payload.display.closed == false {
            (payload.display.client.wl_display_dispatch_pending)(wdisplay);

            let (mut context, event_handler) = payload.context();
            event_handler.as_mut().unwrap().update(&mut context);
            event_handler.as_mut().unwrap().draw(&mut context);

            if let Some((libegl, egl_display, egl_surface)) = &mut opengl {
                (libegl.eglSwapBuffers.unwrap())(*egl_display, *egl_surface);
            } else {
                payload.context.as_mut().unwrap().swap_buffers();
            }
        }

        if vulkan {
            payload.context.as_mut().unwrap().cleanup_backend();
        }
        if !payload.display.egl_window.is_null() {
            let egl = payload.display.egl.as_ref().unwrap();
            (egl.wl_egl_window_destroy)(payload.display.egl_window);
        }
    }

    Some(())
}
