//! Transport abstraction for the control client: plain TCP or rustls-wrapped.
//!
//! `CtlClient` lets `call()` write/read frames against either backing without
//! duplicating the wire-codec dispatch. The TLS variant is boxed because the
//! `StreamOwned<...>` payload is significantly larger than a raw `TcpStream`,
//! and we never construct enough of these for the heap hop to matter.

use rustls::{ClientConnection, StreamOwned};
use std::io::{Read, Write};
use std::net::TcpStream;

pub(super) enum CtlClient {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl CtlClient {
    pub(super) fn is_tls(&self) -> bool {
        matches!(self, CtlClient::Tls(_))
    }
}

impl Read for CtlClient {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            CtlClient::Plain(s) => s.read(buf),
            CtlClient::Tls(s) => s.read(buf),
        }
    }
}

impl Write for CtlClient {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            CtlClient::Plain(s) => s.write(buf),
            CtlClient::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            CtlClient::Plain(s) => s.flush(),
            CtlClient::Tls(s) => s.flush(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    // Story: `CtlClient` unifies plain TCP and TLS-wrapped streams so the
    // wire codec doesn't care which transport is active. The flag bit
    // (`is_tls()`) drives one downstream branch — the Authenticate-or-skip
    // decision in `call.rs`. We can stand up a real TcpListener for the
    // Plain variant and prove the enum carries the right discriminant
    // without spinning up TLS.

    fn pair_loopback() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let connector = thread::spawn(move || TcpStream::connect(addr).unwrap());
        let (server, _) = listener.accept().unwrap();
        let client = connector.join().unwrap();
        (client, server)
    }

    #[test]
    fn plain_variant_reports_is_tls_false() {
        let (client, _server) = pair_loopback();
        let conn = CtlClient::Plain(client);
        assert!(!conn.is_tls(), "plain TCP must report is_tls() == false");
    }

    #[test]
    fn plain_variant_round_trips_bytes() {
        // End-to-end on the Plain arm: write a handful of bytes through
        // CtlClient, read them on the server side. Proves the Read +
        // Write impls actually delegate to the underlying TcpStream.
        let (client, mut server) = pair_loopback();
        let mut conn = CtlClient::Plain(client);
        conn.write_all(b"ping").unwrap();
        conn.flush().unwrap();

        let mut buf = [0u8; 4];
        std::io::Read::read_exact(&mut server, &mut buf).unwrap();
        assert_eq!(&buf, b"ping");

        // Echo back; CtlClient should read it.
        std::io::Write::write_all(&mut server, b"pong").unwrap();
        let mut got = [0u8; 4];
        conn.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"pong");
    }
}
