use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use block2::RcBlock;
use objc2::runtime::{AnyObject, Imp, ProtocolObject, Sel};
use objc2::{ffi, sel};
use objc2_metal::{MTLCommandQueue, MTLDevice, MTLDrawable};
use objc2_quartz_core::CACurrentMediaTime;

struct Sink {
    lines: Option<Arc<Mutex<Vec<String>>>>,
    epoch: f64,
    start: Instant,
    original: Imp,
    proxy: winit::event_loop::EventLoopProxy<crate::UserEvent>,
}

static SINK: OnceLock<Sink> = OnceLock::new();
static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
static PRESENTED: AtomicUsize = AtomicUsize::new(0);
static COMPLETED: AtomicUsize = AtomicUsize::new(0);
static WAKE_ON_PRESENT: AtomicBool = AtomicBool::new(false);
static SKIPPED: Mutex<[usize; 2]> = Mutex::new([0; 2]);
static EXITING: AtomicBool = AtomicBool::new(false);

pub fn begin_exit() {
    EXITING.store(true, Ordering::Release);
}

pub fn arm_exit() -> usize {
    SEQUENCE.load(Ordering::Acquire) + 1
}

fn drawable_in_flight() -> bool {
    SEQUENCE.load(Ordering::Acquire) > COMPLETED.load(Ordering::Acquire)
}

/// Whether a submitted drawable is still waiting for its vsync, so acquiring the next one
/// would block the UI thread. If so, a `Presented` event follows once it is presented.
pub fn wait_for_drawable() -> bool {
    if !drawable_in_flight() {
        return false;
    }
    WAKE_ON_PRESENT.store(true, Ordering::Release);
    if drawable_in_flight() {
        return true;
    }
    WAKE_ON_PRESENT.store(false, Ordering::Release);
    false
}

pub fn feedback() -> (usize, [usize; 2]) {
    (PRESENTED.load(Ordering::Acquire), *SKIPPED.lock().unwrap())
}

fn record_skip(skipped: &mut [usize; 2], sequence: usize) {
    if sequence > skipped[1] {
        *skipped = [skipped[1], sequence];
    } else if sequence < skipped[1] {
        skipped[0] = skipped[0].max(sequence);
    }
}

unsafe extern "C-unwind" fn present_drawable(
    buffer: *mut AnyObject,
    selector: Sel,
    drawable: *mut ProtocolObject<dyn MTLDrawable>,
) {
    let sink = SINK
        .get()
        .expect("presentation hook installed without sink");
    if let Some(drawable) = unsafe { drawable.as_ref() } {
        let sequence = SEQUENCE.fetch_add(1, Ordering::AcqRel);
        let handler = RcBlock::new(move |drawable: NonNull<ProtocolObject<dyn MTLDrawable>>| {
            let time = unsafe { drawable.as_ref() }.presentedTime();
            if time == 0.0 {
                let mut skipped = SKIPPED.lock().unwrap();
                record_skip(&mut skipped, sequence + 1);
            } else {
                PRESENTED.fetch_max(sequence + 1, Ordering::Release);
            }
            let (time, mode) = if time == 0.0 {
                (sink.start.elapsed().as_micros() as i128, "skip")
            } else {
                (((time - sink.epoch) * 1_000_000.0).round() as i128, "vsync")
            };
            if let Some(lines) = &sink.lines {
                lines
                    .lock()
                    .unwrap()
                    .push(format!("presented {sequence} {time} {mode}"));
            }
            COMPLETED.fetch_add(1, Ordering::AcqRel);
            if WAKE_ON_PRESENT.swap(false, Ordering::AcqRel) || EXITING.load(Ordering::Acquire) {
                let _ = sink.proxy.send_event(crate::UserEvent::Presented);
            }
        });
        // Metal copies the block and invokes it after the drawable is presented.
        unsafe { drawable.addPresentedHandler(RcBlock::as_ptr(&handler)) };
    }
    let original: unsafe extern "C-unwind" fn(
        *mut AnyObject,
        Sel,
        *mut ProtocolObject<dyn MTLDrawable>,
    ) = unsafe { std::mem::transmute(sink.original) };
    unsafe { original(buffer, selector, drawable) };
}

pub fn install(
    device: &wgpu::Device,
    start: Instant,
    lines: Option<Arc<Mutex<Vec<String>>>>,
    proxy: winit::event_loop::EventLoopProxy<crate::UserEvent>,
) -> Result<(), &'static str> {
    if SINK.get().is_some() {
        return Err("Metal feedback already installed");
    }
    let device = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }.ok_or("not a Metal device")?;
    let queue = device
        .raw_device()
        .newCommandQueue()
        .ok_or("no Metal command queue")?;
    let buffer = queue.commandBuffer().ok_or("no Metal command buffer")?;
    let object: &AnyObject = (*buffer).as_ref();
    let selector = sel!(presentDrawable:);
    let method = unsafe { ffi::class_getInstanceMethod(object.class(), selector).as_ref() }
        .ok_or("presentDrawable: unavailable")?;
    let sampled = CACurrentMediaTime();
    let epoch = sampled - start.elapsed().as_secs_f64();
    SINK.set(Sink {
        lines: lines.clone(),
        epoch,
        start,
        original: method.implementation(),
        proxy,
    })
    .map_err(|_| "Metal feedback already installed")?;
    // The replacement preserves the Objective-C method's ABI and calls its original IMP.
    unsafe {
        ffi::method_setImplementation(
            method,
            std::mem::transmute::<
                unsafe extern "C-unwind" fn(
                    *mut AnyObject,
                    Sel,
                    *mut ProtocolObject<dyn MTLDrawable>,
                ),
                Imp,
            >(present_drawable),
        );
    }
    if let Some(lines) = lines {
        lines.lock().unwrap().push("source metal-presented".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::record_skip;

    #[test]
    fn skipped_feedback_keeps_two_distinct_sequences_in_any_order() {
        for sequences in [[1, 2, 3, 3], [3, 3, 1, 2], [2, 3, 1, 3]] {
            let mut skipped = [0; 2];
            for sequence in sequences {
                record_skip(&mut skipped, sequence);
            }
            assert_eq!(skipped, [2, 3]);
        }
    }
}
