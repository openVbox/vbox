use std::io::{BufReader, BufWriter};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use vbox_proto::{Hello, Message, PROTOCOL_VERSION};

use crate::client_name;

pub(crate) struct Session {
    pub(crate) reader: BufReader<TcpStream>,
    pub(crate) writer: BufWriter<TcpStream>,
    pub(crate) server_name: String,
    pub(crate) session_id: u64,
    pub(crate) next_seq: u64,
}

pub(crate) fn handshake(addr: SocketAddr, timeout: Duration) -> Result<Session> {
    let stream =
        TcpStream::connect_timeout(&addr, timeout).with_context(|| format!("connect {addr}"))?;
    stream.set_nodelay(true).ok();
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();

    let read_half = stream.try_clone().context("cloning stream")?;
    let mut r = BufReader::new(read_half);
    let mut w = BufWriter::new(stream);

    vbox_proto::write_prelude(&mut w)?;
    let _ver = vbox_proto::read_prelude(&mut r)?;

    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        client_name: client_name(),
    };
    vbox_proto::write_frame(&mut w, &Message::Hello(hello))?;

    let welcome = match vbox_proto::read_frame(&mut r)? {
        Message::Welcome(w) => w,
        Message::Goodbye(g) => bail!("server refused: {}", g.reason),
        Message::Error(e) => bail!("server error {}: {}", e.code, e.message),
        other => bail!("expected Welcome, got {:?}", other.kind()),
    };
    if welcome.protocol_version != PROTOCOL_VERSION {
        bail!(
            "protocol version mismatch: server={}, client={}",
            welcome.protocol_version,
            PROTOCOL_VERSION
        );
    }

    Ok(Session {
        reader: r,
        writer: w,
        server_name: welcome.server_name,
        session_id: welcome.session_id,
        next_seq: 1,
    })
}
