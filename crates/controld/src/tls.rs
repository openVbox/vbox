//! TLS server adapter for vbox-controld. Wraps either a plain TcpStream or a
//! rustls server-side `StreamOwned` behind a single `Read + Write` enum so the
//! protocol layer (`read_frame`, `write_frame`) doesn't care which mode is
//! active. When mTLS is on, the client must present a cert signed by the CA
//! we loaded — that takes the place of the shared-secret token.

use anyhow::{Context, Result, anyhow};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ServerConfig, ServerConnection, WebPkiClientVerifier};
use rustls::{RootCertStore, StreamOwned};
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::Arc;

pub enum CtlConn {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ServerConnection, TcpStream>>),
}

impl CtlConn {
    pub fn peer_addr(&self) -> Option<SocketAddr> {
        match self {
            CtlConn::Plain(s) => s.peer_addr().ok(),
            CtlConn::Tls(s) => s.sock.peer_addr().ok(),
        }
    }

    pub fn is_tls(&self) -> bool {
        matches!(self, CtlConn::Tls(_))
    }
}

impl Read for CtlConn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            CtlConn::Plain(s) => s.read(buf),
            CtlConn::Tls(s) => s.read(buf),
        }
    }
}

impl Write for CtlConn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            CtlConn::Plain(s) => s.write(buf),
            CtlConn::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            CtlConn::Plain(s) => s.flush(),
            CtlConn::Tls(s) => s.flush(),
        }
    }
}

pub struct ServerTls {
    config: Arc<ServerConfig>,
}

impl ServerTls {
    pub fn load(cert_path: &Path, key_path: &Path, client_ca_path: &Path) -> Result<Self> {
        let certs = load_certs(cert_path)
            .with_context(|| format!("load server cert {}", cert_path.display()))?;
        let key = load_private_key(key_path)
            .with_context(|| format!("load server key {}", key_path.display()))?;
        let client_ca_certs = load_certs(client_ca_path)
            .with_context(|| format!("load client CA {}", client_ca_path.display()))?;

        let mut roots = RootCertStore::empty();
        for c in client_ca_certs {
            roots
                .add(c)
                .with_context(|| format!("add cert from {}", client_ca_path.display()))?;
        }
        let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
            .build()
            .context("build client verifier")?;

        let config = ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, key)
            .context("install server cert+key")?;

        Ok(Self {
            config: Arc::new(config),
        })
    }

    pub fn accept(&self, stream: TcpStream) -> Result<CtlConn> {
        let conn =
            ServerConnection::new(Arc::clone(&self.config)).context("new ServerConnection")?;
        Ok(CtlConn::Tls(Box::new(StreamOwned::new(conn, stream))))
    }
}

fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let mut rdr =
        BufReader::new(File::open(path).with_context(|| format!("open {}", path.display()))?);
    rustls_pemfile::certs(&mut rdr)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("parse certs {}", path.display()))
}

fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>> {
    let mut rdr =
        BufReader::new(File::open(path).with_context(|| format!("open {}", path.display()))?);
    rustls_pemfile::private_key(&mut rdr)
        .with_context(|| format!("parse private key {}", path.display()))?
        .ok_or_else(|| anyhow!("no private key in {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{CertifiedKey, generate_simple_self_signed};
    use std::fs;
    use std::path::PathBuf;

    // The operator-facing flow this module supports:
    //   1. `vbox-controld --tls-cert ... --tls-key ... --tls-client-ca ...`
    //      hands ServerTls::load three PEM paths.
    //   2. load() must read+parse each one, complain clearly when something
    //      is missing or malformed, and otherwise return a config ready to
    //      accept connections.
    //
    // Tests build minimal but real PEMs on disk using rcgen (the same crate
    // already in the workspace) and walk both the happy and the error paths.

    fn write_pem_pair(dir: &Path, name: &str) -> (PathBuf, PathBuf) {
        let CertifiedKey { cert, key_pair } =
            generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert_path = dir.join(format!("{name}.crt"));
        let key_path = dir.join(format!("{name}.key"));
        fs::write(&cert_path, cert.pem()).unwrap();
        fs::write(&key_path, key_pair.serialize_pem()).unwrap();
        (cert_path, key_path)
    }

    #[test]
    fn server_tls_loads_a_valid_cert_key_ca_triple() {
        // Operator deploys three real PEMs. load() should swallow them, build
        // a ServerConfig under the hood, and hand us back a ServerTls we can
        // hold onto across sessions.
        let dir = tempdir_for_test();
        let (cert, key) = write_pem_pair(&dir.path, "server");
        let (ca, _ca_key) = write_pem_pair(&dir.path, "client-ca");

        let tls = ServerTls::load(&cert, &key, &ca).expect("valid PEMs must load");

        // Sanity: the ServerConfig was actually populated; we can build a
        // ServerConnection from it. (We don't run a real handshake here —
        // that needs a TcpStream — but the constructor would refuse an
        // empty config, so this proves load() filled it.)
        let conn = ServerConnection::new(Arc::clone(&tls.config));
        assert!(
            conn.is_ok(),
            "ServerConnection from loaded config must build"
        );
    }

    #[test]
    fn load_certs_complains_when_file_missing() {
        let dir = tempdir_for_test();
        let missing = dir.path.join("nope.crt");

        let err = load_certs(&missing).expect_err("missing cert file must error");

        let chain = format!("{err:#}");
        assert!(
            chain.contains("nope.crt"),
            "error must name the missing file, got: {chain}"
        );
    }

    #[test]
    fn load_private_key_errors_when_pem_has_no_key_section() {
        // A common operator mistake: paste the cert PEM into the key path.
        // The file opens fine and rustls-pemfile returns Ok(None); we must
        // turn that into a clear error rather than a panic.
        let dir = tempdir_for_test();
        let (cert, _key) = write_pem_pair(&dir.path, "server");
        let key_path = dir.path.join("not-a-key.pem");
        // Copy only the certificate part — no PRIVATE KEY block.
        fs::copy(&cert, &key_path).unwrap();

        let err = load_private_key(&key_path).expect_err("cert-only PEM is not a key");

        let chain = format!("{err:#}");
        assert!(
            chain.contains("no private key"),
            "error chain should explain 'no private key', got: {chain}"
        );
    }

    #[test]
    fn server_tls_load_rejects_unreadable_cert_path() {
        // Mirrors the operator typo "--tls-cert /etc/vbox/cret.pem". load()
        // must surface the missing-file failure with the original path
        // wrapped in context so logs are actionable.
        let dir = tempdir_for_test();
        let (_, key) = write_pem_pair(&dir.path, "server");
        let (ca, _ca_key) = write_pem_pair(&dir.path, "client-ca");
        let typo = dir.path.join("cret.pem"); // intentional typo, no file

        // ServerTls doesn't implement Debug, so unwrap_err is off the table —
        // pattern-match the Err arm directly.
        let err = match ServerTls::load(&typo, &key, &ca) {
            Ok(_) => panic!("missing cert must fail"),
            Err(e) => e,
        };

        let chain = format!("{err:#}");
        assert!(
            chain.contains("load server cert"),
            "error must say which step failed, got: {chain}"
        );
        assert!(chain.contains("cret.pem"));
    }

    struct TempDir {
        path: PathBuf,
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
    fn tempdir_for_test() -> TempDir {
        let base = std::env::temp_dir();
        let suffix = format!(
            "vbox-controld-tls-{}-{}",
            std::process::id(),
            unique_counter()
        );
        let path = base.join(suffix);
        fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }
    fn unique_counter() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static C: AtomicU64 = AtomicU64::new(0);
        C.fetch_add(1, Ordering::Relaxed)
    }
}
