//! Transport-neutral I/O for the Wayland compositor loop.
//!
//! TCP and QUIC data planes feed inbound protocol messages into the same
//! channel and drain compositor output from the same sender. This keeps the
//! Wayland code independent of the concrete network transport.

use std::io::{BufReader, BufWriter};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use anyhow::{Context, Result};
use vbox_proto::Message;

pub(crate) struct WaylandIo {
    inbound_rx: mpsc::Receiver<Message>,
    outbound_tx: mpsc::Sender<Message>,
    disconnected: Arc<AtomicBool>,
}

impl WaylandIo {
    pub(crate) fn from_parts(
        inbound_rx: mpsc::Receiver<Message>,
        outbound_tx: mpsc::Sender<Message>,
        disconnected: Arc<AtomicBool>,
    ) -> Self {
        Self {
            inbound_rx,
            outbound_tx,
            disconnected,
        }
    }

    pub(crate) fn from_tcp(
        mut reader: BufReader<TcpStream>,
        mut writer: BufWriter<TcpStream>,
    ) -> Result<Self> {
        reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(1)))
            .ok();
        let (inbound_tx, inbound_rx) = mpsc::channel();
        let (outbound_tx, outbound_rx) = mpsc::channel::<Message>();
        let disconnected = Arc::new(AtomicBool::new(false));

        {
            let disconnected = Arc::clone(&disconnected);
            std::thread::Builder::new()
                .name("vbox-tcp-view-read".into())
                .spawn(move || {
                    loop {
                        match vbox_proto::read_frame(&mut reader) {
                            Ok(msg) => {
                                if inbound_tx.send(msg).is_err() {
                                    disconnected.store(true, Ordering::SeqCst);
                                    return;
                                }
                            }
                            Err(e) => {
                                if e.downcast_ref::<std::io::Error>()
                                    .is_some_and(|io| io.kind() == std::io::ErrorKind::WouldBlock)
                                {
                                    continue;
                                }
                                disconnected.store(true, Ordering::SeqCst);
                                return;
                            }
                        }
                    }
                })
                .context("spawning TCP view reader")?;
        }

        {
            let disconnected = Arc::clone(&disconnected);
            std::thread::Builder::new()
                .name("vbox-tcp-view-write".into())
                .spawn(move || {
                    for msg in outbound_rx {
                        if vbox_proto::write_frame(&mut writer, &msg).is_err() {
                            disconnected.store(true, Ordering::SeqCst);
                            return;
                        }
                    }
                    disconnected.store(true, Ordering::SeqCst);
                })
                .context("spawning TCP view writer")?;
        }

        Ok(Self::from_parts(inbound_rx, outbound_tx, disconnected))
    }

    pub(crate) fn disconnected(&self) -> bool {
        self.disconnected.load(Ordering::SeqCst)
    }

    pub(crate) fn try_recv(&self) -> Option<Message> {
        self.inbound_rx.try_recv().ok()
    }

    pub(crate) fn send(&self, msg: Message) -> Result<()> {
        self.outbound_tx
            .send(msg)
            .context("sending compositor message to transport")
    }
}
