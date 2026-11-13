use crate::frame_clock::FrameClock;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wayland_client::protocol::{wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::wp::presentation_time::client::{wp_presentation, wp_presentation_feedback};
use winit::raw_window_handle::{
    HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
};

struct State {
    presentation: Option<wp_presentation::WpPresentation>,
    epoch: Option<i128>,
    start: Instant,
    lines: Option<Arc<Mutex<Vec<String>>>>,
    clock: FrameClock,
    presented: usize,
    discarded: usize,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
            && interface == "wp_presentation"
        {
            state.presentation = Some(registry.bind(name, 1, qh, ()));
        }
    }
}

impl Dispatch<wp_presentation::WpPresentation, ()> for State {
    fn event(
        state: &mut Self,
        _: &wp_presentation::WpPresentation,
        event: wp_presentation::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            let mut clock = std::mem::MaybeUninit::<libc::timespec>::uninit();
            if unsafe { libc::clock_gettime(clk_id as libc::clockid_t, clock.as_mut_ptr()) } == 0 {
                let now = Instant::now();
                let clock = unsafe { clock.assume_init() };
                state.epoch = Some(
                    i128::from(clock.tv_sec) * 1_000_000 + i128::from(clock.tv_nsec) / 1_000
                        - now.duration_since(state.start).as_micros() as i128,
                );
            }
        }
    }
}

impl Dispatch<wp_presentation_feedback::WpPresentationFeedback, usize> for State {
    fn event(
        state: &mut Self,
        _: &wp_presentation_feedback::WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        seq: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                refresh,
                flags,
                ..
            } => {
                state.presented = state.presented.max(*seq + 1);
                if let Some(epoch) = state.epoch {
                    let seconds = (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo);
                    let time =
                        i128::from(seconds) * 1_000_000 + i128::from(tv_nsec) / 1_000 - epoch;
                    if let Ok(micros) = u64::try_from(time) {
                        state.clock.presented(
                            *seq,
                            state.start + Duration::from_micros(micros),
                            Duration::from_nanos(u64::from(refresh)),
                        );
                    }
                    let bits = match flags {
                        wayland_client::WEnum::Value(v) => v.bits(),
                        wayland_client::WEnum::Unknown(v) => v,
                    };
                    let mode = [
                        (1, "vsync"),
                        (2, "hw_clock"),
                        (4, "hw_completion"),
                        (8, "zero_copy"),
                    ]
                    .into_iter()
                    .filter_map(|(mask, name)| (bits & mask != 0).then_some(name))
                    .collect::<Vec<_>>()
                    .join("+");
                    if let Some(lines) = &state.lines {
                        lines.lock().unwrap().push(format!(
                            "presented {seq} {time} {}",
                            if mode.is_empty() { "none" } else { &mode }
                        ));
                    }
                }
            }
            wp_presentation_feedback::Event::Discarded => {
                state.discarded = state.discarded.max(*seq + 1);
                state.clock.discarded(*seq);
                if let Some(lines) = &state.lines {
                    lines.lock().unwrap().push(format!(
                        "presented {seq} {} skip",
                        state.start.elapsed().as_micros()
                    ));
                }
            }
            _ => {}
        }
    }
}

pub struct Feedback {
    presentation: wp_presentation::WpPresentation,
    surface: wl_surface::WlSurface,
    qh: QueueHandle<State>,
    queue: EventQueue<State>,
    state: State,
    _registry: wl_registry::WlRegistry,
    // Keep the foreign display and surface alive until our queue is released.
    _window: Arc<winit::window::Window>,
}

impl Feedback {
    pub fn new(
        window: Arc<winit::window::Window>,
        start: Instant,
        lines: Option<Arc<Mutex<Vec<String>>>>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let RawDisplayHandle::Wayland(display) = window.display_handle()?.as_raw() else {
            return Err("not a Wayland display".into());
        };
        let RawWindowHandle::Wayland(surface) = window.window_handle()?.as_raw() else {
            return Err("not a Wayland surface".into());
        };
        // Guest backend owns only our proxies/queue, never winit's display or surface.
        let backend = unsafe {
            wayland_backend::client::Backend::from_foreign_display(display.display.as_ptr().cast())
        };
        let connection = Connection::from_backend(backend);
        let id = unsafe {
            wayland_backend::client::ObjectId::from_ptr(
                wl_surface::WlSurface::interface(),
                surface.surface.as_ptr().cast(),
            )
        }?;
        let surface = wl_surface::WlSurface::from_id(&connection, id)?;
        let mut queue = connection.new_event_queue::<State>();
        let qh = queue.handle();
        let mut state = State {
            presentation: None,
            epoch: None,
            start,
            lines,
            clock: FrameClock::default(),
            presented: 0,
            discarded: 0,
        };
        let registry = connection.display().get_registry(&qh, ());
        queue.roundtrip(&mut state)?;
        queue.roundtrip(&mut state)?;
        let presentation = state
            .presentation
            .take()
            .ok_or("wp_presentation not advertised")?;
        if state.epoch.is_none() {
            return Err("presentation clock unavailable".into());
        }
        if let Some(lines) = &state.lines {
            lines
                .lock()
                .unwrap()
                .push("source wayland-presentation".into());
        }
        Ok(Self {
            presentation,
            surface,
            qh,
            queue,
            state,
            _registry: registry,
            _window: window,
        })
    }

    pub fn request(&self, sequence: usize) {
        self.presentation
            .feedback(&self.surface, &self.qh, sequence);
    }

    pub fn exit_feedback(&self) -> (usize, usize) {
        (self.state.presented, self.state.discarded)
    }

    pub fn clock(&mut self) -> Option<&mut FrameClock> {
        self.queue.dispatch_pending(&mut self.state).ok()?;
        Some(&mut self.state.clock)
    }
}

impl Drop for Feedback {
    fn drop(&mut self) {
        let _ = self.queue.dispatch_pending(&mut self.state);
        if let Some(lines) = &self.state.lines {
            let (cost, margin, period) = self.state.clock.timings();
            lines.lock().unwrap().push(format!(
                "clock_ns {} {} {}",
                cost.as_nanos(),
                margin.as_nanos(),
                period.as_nanos()
            ));
        }
        self.presentation.destroy();
    }
}
