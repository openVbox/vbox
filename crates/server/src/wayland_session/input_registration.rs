//! Bridge between the TCP input thread and the Wayland event loop.
//!
//! The TCP reader thread receives `InputEvent`s from the host and needs a
//! way to hand them to the Wayland thread that owns `App`. We use a static
//! `mpsc::Sender` slot for that: the Wayland thread arms it with
//! [`InputRegistration::new`] right after creating its receiver, and the
//! TCP thread calls [`send_input`] to forward each event. The slot is
//! reset to `None` in `InputRegistration::Drop` so a server restart
//! doesn't fire events into the previous run's dead receiver.
use std::sync::{Mutex, OnceLock, mpsc};

use vbox_proto::InputEvent;

static INPUT_SENDER: OnceLock<Mutex<Option<mpsc::Sender<InputEvent>>>> = OnceLock::new();

pub fn send_input(event: InputEvent) -> bool {
    let Some(sender) = INPUT_SENDER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
    else {
        return false;
    };
    sender.send(event).is_ok()
}

pub(crate) struct InputRegistration;

impl InputRegistration {
    pub(crate) fn new(sender: mpsc::Sender<InputEvent>) -> Self {
        if let Ok(mut slot) = INPUT_SENDER.get_or_init(|| Mutex::new(None)).lock() {
            *slot = Some(sender);
        }
        Self
    }
}

impl Drop for InputRegistration {
    fn drop(&mut self) {
        if let Ok(mut slot) = INPUT_SENDER.get_or_init(|| Mutex::new(None)).lock() {
            *slot = None;
        }
    }
}
