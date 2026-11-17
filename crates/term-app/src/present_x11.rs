use std::sync::{Arc, Mutex};
use std::time::Instant;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::present::{self, CompleteKind, CompleteMode, EventMask};

pub fn start(
    window: &winit::window::Window,
    start: Instant,
    lines: Arc<Mutex<Vec<String>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let window = match window.window_handle()?.as_raw() {
        RawWindowHandle::Xlib(h) => u32::try_from(h.window)?,
        RawWindowHandle::Xcb(h) => h.window.get(),
        _ => return Err("not an X11 window".into()),
    };
    let (connection, _) = x11rb::connect(None)?;
    let eid = connection.generate_id()?;
    present::select_input(&connection, eid, window, EventMask::COMPLETE_NOTIFY)?.check()?;
    connection.flush()?;
    let mut clock = std::mem::MaybeUninit::<libc::timespec>::uninit();
    // X Present UST uses the same CLOCK_MONOTONIC epoch.
    // SAFETY: clock points to writable storage for one timespec.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, clock.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let sampled = Instant::now();
    // SAFETY: clock_gettime succeeded and initialized every field of clock.
    let clock = unsafe { clock.assume_init() };
    let epoch = i128::from(clock.tv_sec) * 1_000_000 + i128::from(clock.tv_nsec) / 1_000
        - sampled.duration_since(start).as_micros() as i128;
    lines.lock().unwrap().push("source x11-present".into());
    std::thread::Builder::new()
        .name("presentation-feedback".into())
        .spawn(move || {
            let mut sequence = 0;
            while let Ok(event) = connection.wait_for_event() {
                if let Event::PresentCompleteNotify(event) = event {
                    if event.kind != CompleteKind::PIXMAP {
                        continue;
                    }
                    let mode = match event.mode {
                        CompleteMode::FLIP => "flip",
                        CompleteMode::COPY => "copy",
                        CompleteMode::SKIP => "skip",
                        CompleteMode::SUBOPTIMAL_COPY => "suboptimal-copy",
                        _ => "unknown",
                    };
                    let time = i128::from(event.ust) - epoch;
                    let mut lines = lines.lock().unwrap();
                    lines.push(format!("presented {sequence} {time} {mode}"));
                    lines.push(format!("serial {sequence} {}", event.serial));
                    sequence += 1;
                }
            }
        })?;
    Ok(())
}
