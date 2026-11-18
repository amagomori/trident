//! Receive copy-only file drops without blocking Wayland event dispatch.

use std::os::fd::AsFd;
use std::sync::atomic::{AtomicBool, Ordering};

use calloop::{generic::Generic, Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use rustix::io::Errno;
use sctk::reexports::client::backend::ObjectId;
use sctk::reexports::client::protocol::{
    wl_data_device::{self, WlDataDevice},
    wl_data_device_manager::{DndAction, WlDataDeviceManager},
    wl_data_offer::{self, WlDataOffer},
    wl_seat::WlSeat,
    wl_surface::WlSurface,
};
use sctk::reexports::client::{
    delegate_noop, event_created_child, Connection, Dispatch, Proxy, QueueHandle, WEnum,
};

use crate::event::WindowEvent;
use crate::platform_impl::common::uri_list::parse_uri_list;
use crate::platform_impl::wayland::{make_wid, state::WinitState};

const URI_LIST: &str = "text/uri-list";
const MAX_OFFER_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub(super) struct DropDevice {
    device: WlDataDevice,
    target: Option<(Offer, Option<WlSurface>)>,
    // A stalled source may keep its write end indefinitely. Bound reads per seat.
    pending: Option<(LoopHandle<'static, WinitState>, RegistrationToken)>,
}

impl DropDevice {
    pub(super) fn new(
        manager: &WlDataDeviceManager,
        qh: &QueueHandle<WinitState>,
        seat: &WlSeat,
    ) -> Self {
        Self { device: manager.get_data_device(seat, qh, seat.id()), target: None, pending: None }
    }
}

impl Drop for DropDevice {
    fn drop(&mut self) {
        self.cancel_read();
        if self.device.version() >= 2 {
            self.device.release();
        }
    }
}

#[derive(Debug)]
struct Offer(WlDataOffer);

impl Drop for Offer {
    fn drop(&mut self) {
        self.0.destroy();
    }
}

#[derive(Default)]
struct OfferData {
    uri_list: AtomicBool,
    copy: AtomicBool,
}

impl Dispatch<WlDataOffer, OfferData> for WinitState {
    fn event(
        _: &mut Self,
        _: &WlDataOffer,
        event: wl_data_offer::Event,
        data: &OfferData,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_offer::Event::Offer { mime_type } if mime_type == URI_LIST => {
                data.uri_list.store(true, Ordering::Relaxed);
            },
            wl_data_offer::Event::Action { dnd_action } => {
                data.copy.store(dnd_action == WEnum::Value(DndAction::Copy), Ordering::Relaxed);
            },
            _ => (),
        }
    }
}

delegate_noop!(WinitState: WlDataDeviceManager);

impl Dispatch<WlDataDevice, ObjectId> for WinitState {
    event_created_child!(WinitState, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, OfferData::default())
    ]);

    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        seat: &ObjectId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let device = state.seats.get_mut(seat).and_then(|s| s.drop_device.as_mut());
        match event {
            wl_data_device::Event::Enter { id, serial, surface, .. } => {
                let offer = id.map(Offer);
                let Some(device) = device else { return };
                device.target = None;
                if let Some(offer) = offer {
                    if state.windows.get_mut().contains_key(&make_wid(&surface))
                        && offer.0.data::<OfferData>().unwrap().uri_list.load(Ordering::Relaxed)
                    {
                        offer.0.accept(serial, Some(URI_LIST.into()));
                        if offer.0.version() >= 3 {
                            offer.0.set_actions(DndAction::Copy, DndAction::Copy);
                        }
                        device.target = Some((offer, Some(surface)));
                    } else {
                        offer.0.accept(serial, None);
                        // Keep the declined offer alive until Leave; destroying it now can
                        // cancel the source drag before it reaches another destination.
                        device.target = Some((offer, None));
                    }
                }
            },
            wl_data_device::Event::Leave => {
                if let Some(device) = device {
                    device.target = None;
                }
            },
            wl_data_device::Event::Drop => {
                let Some(device) = device else { return };
                if let Some((offer, Some(surface))) = device.target.take() {
                    if offer.0.version() < 3
                        || offer.0.data::<OfferData>().unwrap().copy.load(Ordering::Relaxed)
                    {
                        device.receive_drop(&state.loop_handle, offer, surface);
                    }
                }
            },
            // Clipboard ownership is handled independently of file drops.
            wl_data_device::Event::Selection { id: Some(offer) } => offer.destroy(),
            _ => (),
        }
    }
}

impl DropDevice {
    fn cancel_read(&mut self) {
        if let Some((handle, token)) = self.pending.take() {
            handle.remove(token);
        }
    }

    fn receive_drop(
        &mut self,
        handle: &LoopHandle<'static, WinitState>,
        offer: Offer,
        surface: WlSurface,
    ) {
        self.cancel_read();
        let Ok((read, write)) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC) else {
            return;
        };
        if rustix::io::ioctl_fionbio(&read, true).is_err() {
            return;
        }
        offer.0.receive(URI_LIST.into(), write.as_fd());
        let mut data = Vec::new();
        let reader = Generic::new(read, Interest::READ, Mode::Level);
        self.pending = handle
            .insert_source(reader, move |_, fd, state| {
                let mut buf = [0u8; 8192];
                match rustix::io::read(&*fd, &mut buf) {
                    Ok(0) => {
                        let window_id = make_wid(&surface);
                        if surface.is_alive() && state.windows.get_mut().contains_key(&window_id) {
                            if let Ok(paths) = parse_uri_list(&data) {
                                for path in paths {
                                    state.events_sink.push_window_event(
                                        WindowEvent::DroppedFile(path),
                                        window_id,
                                    );
                                }
                                state.dispatched_events = true;
                                if offer.0.version() >= 3 {
                                    offer.0.finish();
                                }
                            }
                        }
                    },
                    Ok(n) if data.len() + n <= MAX_OFFER_BYTES => {
                        data.extend_from_slice(&buf[..n]);
                        return Ok(PostAction::Continue);
                    },
                    Err(Errno::INTR | Errno::AGAIN) => return Ok(PostAction::Continue),
                    _ => (),
                }
                Ok(PostAction::Remove)
            })
            .ok()
            .map(|token| (handle.clone(), token));
    }
}
