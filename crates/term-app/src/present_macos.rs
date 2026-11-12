use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use block2::RcBlock;
use objc2::runtime::{AnyObject, Imp, ProtocolObject, Sel};
use objc2::{ffi, sel};
use objc2_metal::{MTLCommandQueue, MTLDevice, MTLDrawable};
use objc2_quartz_core::CACurrentMediaTime;

struct Sink {
    lines: Arc<Mutex<Vec<String>>>,
    epoch: f64,
    start: Instant,
    original: Imp,
}

static SINK: OnceLock<Sink> = OnceLock::new();
static SEQUENCE: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C-unwind" fn present_drawable(
    buffer: *mut AnyObject,
    selector: Sel,
    drawable: *mut ProtocolObject<dyn MTLDrawable>,
) {
    let sink = SINK
        .get()
        .expect("presentation hook installed without sink");
    if let Some(drawable) = unsafe { drawable.as_ref() } {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let handler = RcBlock::new(move |drawable: NonNull<ProtocolObject<dyn MTLDrawable>>| {
            let time = unsafe { drawable.as_ref() }.presentedTime();
            let (time, mode) = if time == 0.0 {
                (sink.start.elapsed().as_micros() as i128, "skip")
            } else {
                (((time - sink.epoch) * 1_000_000.0).round() as i128, "vsync")
            };
            sink.lines
                .lock()
                .unwrap()
                .push(format!("presented {sequence} {time} {mode}"));
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
    lines: Arc<Mutex<Vec<String>>>,
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
    lines.lock().unwrap().push("source metal-presented".into());
    Ok(())
}
