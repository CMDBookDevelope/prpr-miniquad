//! Real AppKit window smoke. Run the two isolated cases with no arguments.
#[cfg(target_os = "macos")]
mod smoke {
    use miniquad::{
        conf::{Conf, Platform},
        native::apple::frameworks::*,
        Context, EventHandler, KeyCode, KeyMods, PassAction,
    };
    use std::{
        fs::{self, File},
        path::PathBuf,
        process::{Command, Stdio},
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    const PASS: &str = "PHI_MAC_PREVIEW_SMOKE_PASS";
    const FRAMES: u32 = 200;

    #[derive(Default)]
    struct State {
        updates: u32,
        draws: u32,
        initial: Option<(f32, f32)>,
        resize_requested: bool,
        resized: Option<(f32, f32)>,
        input_posted: bool,
        input_seen: bool,
        quit_called: bool,
        dropped: bool,
    }

    struct Handler {
        state: Arc<Mutex<State>>,
        input: bool,
    }

    impl EventHandler for Handler {
        fn update(&mut self, ctx: &mut Context) {
            let (resize, finish) = {
                let mut state = self.state.lock().unwrap();
                state.updates += 1;
                let size = ctx.screen_size();
                if state.initial.is_none() && size.0 > 0. && size.1 > 0. {
                    state.initial = Some(size);
                }
                let resize =
                    state.draws >= 20 && state.initial.is_some() && !state.resize_requested;
                if resize {
                    state.resize_requested = true;
                }
                let finish = state.draws >= FRAMES && state.resized.is_some() && !state.quit_called;
                if finish && !self.input {
                    state.quit_called = true;
                }
                (resize, finish)
            };
            // The real resize callback can reenter the handler synchronously.
            // Do not hold the state mutex across either native operation.
            if resize {
                eprintln!("SMOKE_RESIZE_REQUEST 480x320");
                ctx.set_window_size(480, 320);
            }
            if finish && !self.input {
                eprintln!("SMOKE_ORDER_QUIT update");
                ctx.order_quit();
            } else if finish && self.input {
                let post = {
                    let mut state = self.state.lock().unwrap();
                    let post = !state.input_posted;
                    state.input_posted = true;
                    post
                };
                if post {
                    unsafe { post_space_to_own_window() };
                }
            }
        }

        fn draw(&mut self, ctx: &mut Context) {
            let phase = self.state.lock().unwrap().draws % 3;
            ctx.begin_default_pass(PassAction::clear_color(
                0.1 + phase as f32 * 0.1,
                0.2,
                0.3,
                1.,
            ));
            ctx.end_render_pass();
            ctx.commit_frame();
            self.state.lock().unwrap().draws += 1;
        }

        fn resize_event(&mut self, ctx: &mut Context, width: f32, height: f32) {
            let mut state = self.state.lock().unwrap();
            if state.resize_requested && width > 0. && height > 0. {
                if Some((width, height)) != state.initial {
                    let size = ctx.screen_size();
                    assert!((size.0 - width).abs() < 1. && (size.1 - height).abs() < 1.);
                    state.resized = Some((width, height));
                    eprintln!("SMOKE_RESIZE_CALLBACK {width}x{height}");
                }
            }
        }

        fn key_down_event(&mut self, ctx: &mut Context, key: KeyCode, _: KeyMods, repeat: bool) {
            if self.input && key == KeyCode::Space && !repeat {
                let mut state = self.state.lock().unwrap();
                state.input_seen = true;
                state.quit_called = true;
                drop(state);
                eprintln!("SMOKE_ORDER_QUIT input");
                ctx.order_quit();
            }
        }
    }

    impl Drop for Handler {
        fn drop(&mut self) {
            self.state.lock().unwrap().dropped = true;
            eprintln!("SMOKE_HANDLER_DROP");
        }
    }

    // Post a public NSEvent to this application's own queue. This exercises
    // miniquad's real input dispatch without global CGEvent/TCC permissions.
    unsafe fn post_space_to_own_window() {
        let app: ObjcId = msg_send![class!(NSApplication), sharedApplication];
        let mut window: ObjcId = msg_send![app, keyWindow];
        if window.is_null() {
            window = msg_send![app, mainWindow];
        }
        assert!(!window.is_null(), "No smoke window can receive input");
        let number: i64 = msg_send![window, windowNumber];
        let text: ObjcId = msg_send![class!(NSString),
            stringWithUTF8String: b" \0".as_ptr().cast::<std::os::raw::c_char>()];
        let event: ObjcId = msg_send![class!(NSEvent),
            keyEventWithType: NSEventType::NSKeyDown as u64
            location: NSPoint { x: 0., y: 0. }
            modifierFlags: 0u64
            timestamp: 0f64
            windowNumber: number
            context: nil
            characters: text
            charactersIgnoringModifiers: text
            isARepeat: NO
            keyCode: 49u16];
        assert!(!event.is_null(), "Failed to create the smoke input event");
        let () = msg_send![app, postEvent: event atStart: YES];
        eprintln!("SMOKE_INPUT_POSTED");
    }

    fn window_case(mode: &str) {
        let input = mode == "input";
        let state = Arc::new(Mutex::new(State::default()));
        let handler_state = Arc::clone(&state);
        eprintln!("SMOKE_WINDOW_START {mode}");
        miniquad::start(
            Conf {
                window_title: format!("miniquad preview smoke: {mode}"),
                window_width: 320,
                window_height: 200,
                high_dpi: false,
                sample_count: 1,
                platform: Platform {
                    swap_interval: Some(0),
                    ..Default::default()
                },
                ..Default::default()
            },
            move |_| {
                Box::new(Handler {
                    state: handler_state,
                    input,
                })
            },
        );
        let state = state.lock().unwrap();
        assert!(state.updates >= FRAMES && state.draws >= FRAMES);
        assert!(state.resize_requested && state.resized.is_some());
        assert!(
            state.quit_called && state.dropped,
            "start returned without handler Drop"
        );
        assert!(!input || (state.input_posted && state.input_seen));
        let resized = state.resized.unwrap();
        println!("{PASS} {{\"mode\":\"{mode}\",\"updates\":{},\"draws\":{},\"resize\":[{},{}],\"input\":{},\"dropped\":true}}",
            state.updates, state.draws, resized.0, resized.1, state.input_seen);
    }

    fn suite() {
        let folder = std::env::var_os("PHI_TKC_APPLE_SMOKE_ARTIFACTS")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("miniquad-preview-smoke"));
        fs::create_dir_all(&folder).unwrap();
        let began = Instant::now();
        for mode in ["update", "input"] {
            let path = folder.join(format!("macos-preview-{mode}.log"));
            let log = File::create(&path).unwrap();
            let mut child = Command::new(std::env::current_exe().unwrap())
                .arg(mode)
                .stdout(Stdio::from(log.try_clone().unwrap()))
                .stderr(Stdio::from(log))
                .spawn()
                .unwrap();
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if began.elapsed() >= Duration::from_secs(20) {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "Preview smoke timed out; log: {}\n{}",
                        path.display(),
                        fs::read_to_string(&path).unwrap_or_default()
                    );
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            let output = fs::read_to_string(&path).unwrap();
            print!("{output}");
            assert!(
                status.success(),
                "Preview {mode} failed: {status}; log: {}",
                path.display()
            );
            // AppKit termination before start returns can exit with code zero.
            // Require the marker emitted after our Drop and return assertions.
            assert!(
                output.lines().any(|line| line.starts_with(PASS)),
                "Preview {mode} did not return/Drop; log: {}",
                path.display()
            );
        }
    }

    pub fn main() {
        match std::env::args().nth(1).as_deref() {
            None => suite(),
            Some(mode @ ("update" | "input")) => window_case(mode),
            Some(mode) => panic!("Unknown smoke mode: {mode}"),
        }
    }
}

#[cfg(target_os = "macos")]
fn main() {
    smoke::main();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("macos_preview_smoke requires an actual macOS window server");
    std::process::exit(2);
}
