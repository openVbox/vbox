#[cfg(not(target_os = "linux"))]
use std::io::BufWriter;
#[cfg(not(target_os = "linux"))]
use std::net::TcpStream;

#[cfg(not(target_os = "linux"))]
use anyhow::Result;
#[cfg(not(target_os = "linux"))]
use vbox_proto::Message;

#[cfg(not(target_os = "linux"))]
pub(crate) struct WaylandIo {
    writer: std::sync::Mutex<BufWriter<TcpStream>>,
}

#[cfg(not(target_os = "linux"))]
impl WaylandIo {
    pub(crate) fn from_tcp_writer_only(writer: BufWriter<TcpStream>) -> Result<Self> {
        Ok(Self {
            writer: std::sync::Mutex::new(writer),
        })
    }

    pub(crate) fn send(&self, msg: Message) -> Result<()> {
        let mut writer = self.writer.lock().expect("writer mutex");
        vbox_proto::write_frame(&mut *writer, &msg)
    }
}
