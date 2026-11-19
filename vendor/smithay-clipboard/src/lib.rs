//! Smithay Clipboard
//!
//! Provides access to the Wayland clipboard for gui applications. The user
//! should have surface around.

#![deny(clippy::all, clippy::if_not_else, clippy::enum_glob_use)]
use std::collections::HashMap;
use std::ffi::c_void;
use std::io::Result;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, Weak};

use sctk::reexports::calloop::channel::{self, Sender};
use sctk::reexports::client::Connection;
use sctk::reexports::client::backend::Backend;

mod mime;
mod state;
mod worker;

type DropCallback = dyn Fn(&[u8]) -> bool + Send + Sync;

/// A surface's file-drop registration. Dropping it invalidates pending transfers
/// for that surface, even if another window later reuses the same address.
#[must_use]
pub struct FileDropTarget {
    _callback: Arc<DropCallback>,
}

#[derive(Clone, Default)]
pub(crate) struct FileDrops(Arc<Mutex<HashMap<usize, Weak<DropCallback>>>>);

impl FileDrops {
    fn register(&self, surface: usize, callback: Arc<DropCallback>) -> FileDropTarget {
        let mut targets = self.0.lock().unwrap();
        targets.retain(|_, target| target.strong_count() != 0);
        targets.insert(surface, Arc::downgrade(&callback));
        FileDropTarget { _callback: callback }
    }

    fn target(&self, surface: usize) -> Option<Weak<DropCallback>> {
        self.0.lock().unwrap().get(&surface).filter(|t| t.strong_count() != 0).cloned()
    }
}

/// Access to a Wayland clipboard.
pub struct Clipboard {
    file_drops: FileDrops,
    request_sender: Sender<worker::Command>,
    request_receiver: Receiver<Result<String>>,
    clipboard_thread: Option<std::thread::JoinHandle<()>>,
}

impl Clipboard {
    /// Creates new clipboard which will be running on its own thread with its
    /// own event queue to handle clipboard requests.
    ///
    /// # Safety
    ///
    /// `display` must be a valid `*mut wl_display` pointer, and it must remain
    /// valid for as long as `Clipboard` object is alive.
    pub unsafe fn new(display: *mut c_void) -> Self {
        let backend = unsafe { Backend::from_foreign_display(display.cast()) };
        let connection = Connection::from_backend(backend);

        // Create channel to send data to clipboard thread.
        let (request_sender, rx_chan) = channel::channel();
        // Create channel to get data from the clipboard thread.
        let (clipboard_reply_sender, request_receiver) = mpsc::channel();

        let name = String::from("smithay-clipboard");
        let file_drops = FileDrops::default();
        let clipboard_thread =
            worker::spawn(name, connection, rx_chan, clipboard_reply_sender, file_drops.clone());

        Self { request_receiver, request_sender, clipboard_thread, file_drops }
    }

    /// Receives copy-only `text/uri-list` drops on the given Wayland surface.
    ///
    /// The address identifies a live `wl_surface` on this display. The callback
    /// runs on the clipboard worker; return true after accepting the payload.
    pub fn on_file_drop(
        &self,
        surface: usize,
        callback: impl Fn(&[u8]) -> bool + Send + Sync + 'static,
    ) -> FileDropTarget {
        self.file_drops.register(surface, Arc::new(callback))
    }

    /// Load clipboard data.
    ///
    /// Loads content from a clipboard on a last observed seat.
    pub fn load(&self) -> Result<String> {
        let _ = self.request_sender.send(worker::Command::Load);

        if let Ok(reply) = self.request_receiver.recv() {
            reply
        } else {
            // The clipboard thread is dead, however we shouldn't crash downstream, so
            // propogating an error.
            Err(std::io::Error::other("clipboard is dead."))
        }
    }

    /// Store to a clipboard.
    ///
    /// Stores to a clipboard on a last observed seat.
    pub fn store<T: Into<String>>(&self, text: T) {
        let request = worker::Command::Store(text.into());
        let _ = self.request_sender.send(request);
    }

    /// Load primary clipboard data.
    ///
    /// Loads content from a  primary clipboard on a last observed seat.
    pub fn load_primary(&self) -> Result<String> {
        let _ = self.request_sender.send(worker::Command::LoadPrimary);

        if let Ok(reply) = self.request_receiver.recv() {
            reply
        } else {
            // The clipboard thread is dead, however we shouldn't crash downstream, so
            // propogating an error.
            Err(std::io::Error::other("clipboard is dead."))
        }
    }

    /// Store to a primary clipboard.
    ///
    /// Stores to a primary clipboard on a last observed seat.
    pub fn store_primary<T: Into<String>>(&self, text: T) {
        let request = worker::Command::StorePrimary(text.into());
        let _ = self.request_sender.send(request);
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        // Shutdown smithay-clipboard.
        let _ = self.request_sender.send(worker::Command::Exit);
        if let Some(clipboard_thread) = self.clipboard_thread.take() {
            let _ = clipboard_thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_read_cannot_follow_a_reused_surface_address() {
        let drops = FileDrops::default();
        let original = drops.register(1, Arc::new(|bytes| bytes == b"original"));
        let _survivor = drops.register(2, Arc::new(|bytes| bytes == b"survivor"));
        let pending = drops.target(1).unwrap();
        assert!(pending.upgrade().unwrap()(b"original"));
        drop(original);
        assert!(drops.target(1).is_none());
        let _replacement = drops.register(1, Arc::new(|bytes| bytes == b"new"));
        assert!(pending.upgrade().is_none());
        assert!(drops.target(1).unwrap().upgrade().unwrap()(b"new"));
        assert!(drops.target(2).unwrap().upgrade().unwrap()(b"survivor"));
    }
}
