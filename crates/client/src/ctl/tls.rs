//! mTLS plumbing for the control client.
//!
//! - `load_client_tls` builds a rustls `ClientConfig` from `VBOX_TLS_CA/CERT/KEY`.
//! - `tls_bootstrap` is the user-facing `vbox controld-install`-style helper
//!   that mints a CA + server + client cert under a chosen output directory.
//!
//! rcgen 0.13: build a CA with `Issuer`, then sign server/client leafs against
//! it. Keys are P-256 by default (small, fast, ARM64-friendly). We chmod 0600
//! every *.key.pem after writing.

use anyhow::{Context, Result, anyhow, bail};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

/// Returns Some((config, server_name)) when VBOX_TLS_CA/CERT/KEY are all set.
/// Returns None when any of those vars is unset (caller falls back to plain
/// TCP + shared-secret).
pub(super) fn load_client_tls() -> Result<Option<(Arc<ClientConfig>, ServerName<'static>)>> {
    let triple = validate_tls_env_triple(
        crate::brand::env_var("VBOX_TLS_CA").as_deref(),
        crate::brand::env_var("VBOX_TLS_CERT").as_deref(),
        crate::brand::env_var("VBOX_TLS_KEY").as_deref(),
    )?;
    let Some((ca, cert, key)) = triple else {
        return Ok(None);
    };
    let server_name_raw =
        resolve_tls_server_name(crate::brand::env_var("VBOX_TLS_SERVER_NAME").as_deref());

    let ca_certs = load_certs(Path::new(&ca)).with_context(|| format!("load CA {ca}"))?;
    let mut roots = RootCertStore::empty();
    for c in ca_certs {
        roots
            .add(c)
            .with_context(|| format!("add cert from {ca}"))?;
    }
    let client_certs =
        load_certs(Path::new(&cert)).with_context(|| format!("load client cert {cert}"))?;
    let client_key =
        load_private_key(Path::new(&key)).with_context(|| format!("load client key {key}"))?;

    let cfg = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(client_certs, client_key)
        .context("install client cert+key")?;
    let server_name = ServerName::try_from(server_name_raw.clone())
        .map_err(|e| anyhow!("invalid VBOX_TLS_SERVER_NAME={server_name_raw}: {e}"))?;
    Ok(Some((Arc::new(cfg), server_name)))
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

// ── tls-bootstrap helper ────────────────────────────────────────────────────

pub(super) fn tls_bootstrap(sans: &[String], out_dir: &Path) -> Result<()> {
    use rcgen::{
        BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
        KeyUsagePurpose,
    };
    std::fs::create_dir_all(out_dir).with_context(|| format!("create {}", out_dir.display()))?;

    let ca_cert_path = out_dir.join("ca.pem");
    let ca_key_path = out_dir.join("ca.key.pem");

    // Load existing CA or freshly mint one. We keep both as PEM so the CA can
    // survive across host reinstalls — losing the CA invalidates every server
    // and client cert it signed.
    let (ca_cert, ca_key) = if ca_cert_path.exists() && ca_key_path.exists() {
        let cert_pem = std::fs::read_to_string(&ca_cert_path)
            .with_context(|| format!("read {}", ca_cert_path.display()))?;
        let key_pem = std::fs::read_to_string(&ca_key_path)
            .with_context(|| format!("read {}", ca_key_path.display()))?;
        let key = KeyPair::from_pem(&key_pem).context("parse CA key")?;
        // Re-derive a Certificate from the persisted params so we can sign
        // leaves. self_signed against the same key reproduces the on-disk
        // cert (deterministic given the same params + key).
        let params = CertificateParams::from_ca_cert_pem(&cert_pem).context("parse CA cert")?;
        let cert = params.self_signed(&key).context("rebuild CA cert")?;
        (cert, key)
    } else {
        let mut params = CertificateParams::new(Vec::<String>::new()).context("CA params")?;
        params
            .distinguished_name
            .push(DnType::CommonName, "vbox-ca");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let key = KeyPair::generate().context("generate CA key")?;
        let cert = params.self_signed(&key).context("self-sign CA")?;
        std::fs::write(&ca_cert_path, cert.pem())?;
        write_secret(&ca_key_path, &key.serialize_pem())?;
        eprintln!("created CA: {}", ca_cert_path.display());
        (cert, key)
    };

    // Server cert
    {
        let mut params = CertificateParams::new(sans.to_vec()).context("server cert SANs")?;
        params
            .distinguished_name
            .push(DnType::CommonName, "vbox-controld");
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        let key = KeyPair::generate().context("generate server key")?;
        let cert = params
            .signed_by(&key, &ca_cert, &ca_key)
            .context("sign server cert")?;
        let cert_path = out_dir.join("server.pem");
        let key_path = out_dir.join("server.key.pem");
        std::fs::write(&cert_path, cert.pem())?;
        write_secret(&key_path, &key.serialize_pem())?;
        eprintln!(
            "created server cert: {} (SAN: {})",
            cert_path.display(),
            sans.join(", ")
        );
    }

    // Client cert
    {
        let mut params =
            CertificateParams::new(vec!["vbox-host".to_string()]).context("client cert SANs")?;
        params
            .distinguished_name
            .push(DnType::CommonName, "vbox-host");
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        let key = KeyPair::generate().context("generate client key")?;
        let cert = params
            .signed_by(&key, &ca_cert, &ca_key)
            .context("sign client cert")?;
        let cert_path = out_dir.join("client.pem");
        let key_path = out_dir.join("client.key.pem");
        std::fs::write(&cert_path, cert.pem())?;
        write_secret(&key_path, &key.serialize_pem())?;
        eprintln!("created client cert: {}", cert_path.display());
    }

    eprintln!();
    eprintln!("Next steps:");
    eprintln!("  1) Copy these to the guest (e.g. ~/vbox/.vbox-controld/tls/):");
    eprintln!("       {}", out_dir.join("server.pem").display());
    eprintln!("       {}", out_dir.join("server.key.pem").display());
    eprintln!("       {}", out_dir.join("ca.pem").display());
    eprintln!("  2) Restart vbox-controld with:");
    eprintln!("       --tls-cert <server.pem> --tls-key <server.key.pem> --tls-client-ca <ca.pem>");
    eprintln!("  3) On host, export before invoking vbox:");
    eprintln!(
        "       export VBOX_TLS_CA={}",
        out_dir.join("ca.pem").display()
    );
    eprintln!(
        "       export VBOX_TLS_CERT={}",
        out_dir.join("client.pem").display()
    );
    eprintln!(
        "       export VBOX_TLS_KEY={}",
        out_dir.join("client.key.pem").display()
    );
    eprintln!("       export VBOX_TLS_SERVER_NAME=vbox-controld");
    Ok(())
}

fn write_secret(path: &Path, content: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, content).with_context(|| format!("write {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod {}", path.display()))?;
    Ok(())
}

/// Pure resolver for the TLS server-name override. When the env var
/// holds a non-empty value, that wins; otherwise we fall back to the
/// canonical "vbox-controld" SAN that `tls_bootstrap` writes into the
/// daemon's server cert. Trimming guards against `VBOX_TLS_SERVER_NAME=
/// ` (operator typo: trailing space) silently producing an empty SNI.
fn resolve_tls_server_name(value: Option<&str>) -> String {
    value
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "vbox-controld".to_string())
}

/// Pure validator for the VBOX_TLS_CA / VBOX_TLS_CERT / VBOX_TLS_KEY triple.
/// Three documented outcomes:
///   - all three set → `Some((ca, cert, key))`
///   - all three unset → `None` (caller falls back to shared-secret)
///   - partial set → error so the operator notices the half-config
fn validate_tls_env_triple(
    ca: Option<&str>,
    cert: Option<&str>,
    key: Option<&str>,
) -> Result<Option<(String, String, String)>> {
    match (ca, cert, key) {
        (Some(a), Some(c), Some(k)) => Ok(Some((a.to_owned(), c.to_owned(), k.to_owned()))),
        (None, None, None) => Ok(None),
        _ => bail!("VBOX_TLS_CA, VBOX_TLS_CERT, VBOX_TLS_KEY must all be set or all unset"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    // ---- validate_tls_env_triple ------------------------------------------
    //
    // Story: operators either go full mTLS (CA+CERT+KEY) or stay on the
    // shared-secret path. Forgetting one of the three is the most common
    // misconfig, so we surface it as a hard error instead of silently
    // downgrading.

    #[test]
    fn triple_unset_means_no_tls() {
        let out = validate_tls_env_triple(None, None, None).unwrap();
        assert!(out.is_none(), "no env → shared-secret fallback");
    }

    #[test]
    fn triple_fully_set_returns_paths() {
        let out = validate_tls_env_triple(Some("/ca"), Some("/cert"), Some("/key")).unwrap();
        assert_eq!(
            out,
            Some(("/ca".to_owned(), "/cert".to_owned(), "/key".to_owned()))
        );
    }

    #[test]
    fn triple_partial_errors() {
        for (a, c, k) in [
            (Some("/ca"), None, None),
            (None, Some("/cert"), None),
            (None, None, Some("/key")),
            (Some("/ca"), Some("/cert"), None),
            (Some("/ca"), None, Some("/key")),
            (None, Some("/cert"), Some("/key")),
        ] {
            let err = validate_tls_env_triple(a, c, k).expect_err("partial triple must error");
            assert!(format!("{err}").contains("all be set or all unset"));
        }
    }

    // ---- tls_bootstrap end-to-end -----------------------------------------
    //
    // Story: a fresh host runs `vbox ctl tls-bootstrap --san vbox-guest`.
    // Output dir gets ca.pem, server.pem (+key), client.pem (+key), each
    // key chmod 0600. Re-running the same command must reuse the CA but
    // re-sign the leaves — losing the CA would invalidate every server's
    // trust chain.

    #[test]
    fn tls_bootstrap_creates_full_trust_bundle() {
        let dir = tempdir_for_test();
        let out = dir.path.join("tls");

        tls_bootstrap(&["vbox-guest".to_string()], &out).expect("bootstrap must succeed");

        for name in [
            "ca.pem",
            "ca.key.pem",
            "server.pem",
            "server.key.pem",
            "client.pem",
            "client.key.pem",
        ] {
            let p = out.join(name);
            assert!(p.is_file(), "missing {name}");
        }
        // Every *.key.pem must be chmod 0600 — leaking the CA key
        // unwinds the whole deployment, so the test pins the mode.
        for key in ["ca.key.pem", "server.key.pem", "client.key.pem"] {
            let mode = std::fs::metadata(out.join(key))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "{key} must be 0600, got {:o}",
                mode & 0o777
            );
        }
    }

    // ---- resolve_tls_server_name ----------------------------------------
    //
    // Story: by default the daemon's server cert carries
    // `CN=vbox-controld`. Operators with a custom DNS deployment can
    // override the SNI with VBOX_TLS_SERVER_NAME. We pin the precedence
    // (env wins, blank → default) so an empty/spaces value doesn't slip
    // through and break the TLS handshake.

    #[test]
    fn server_name_falls_back_when_unset() {
        assert_eq!(resolve_tls_server_name(None), "vbox-controld");
    }

    #[test]
    fn server_name_falls_back_when_blank() {
        // Common .env mistake: trailing space or only whitespace.
        assert_eq!(resolve_tls_server_name(Some("")), "vbox-controld");
        assert_eq!(resolve_tls_server_name(Some("   ")), "vbox-controld");
    }

    #[test]
    fn server_name_uses_env_value_when_set() {
        assert_eq!(
            resolve_tls_server_name(Some("guest.internal")),
            "guest.internal"
        );
        // Whitespace around a real value is trimmed — `export VAR=" host "`
        // is operator user-error we forgive.
        assert_eq!(
            resolve_tls_server_name(Some("  guest.internal  ")),
            "guest.internal"
        );
    }

    #[test]
    fn tls_bootstrap_is_idempotent_on_ca() {
        // Run twice; the CA cert+key files must be byte-identical the second
        // time. The leaves (server/client) are allowed to differ because
        // each invocation generates fresh leaf keypairs.
        let dir = tempdir_for_test();
        let out = dir.path.join("tls");
        tls_bootstrap(&["vbox-guest".to_string()], &out).unwrap();
        let ca_first = std::fs::read(out.join("ca.pem")).unwrap();
        let ca_key_first = std::fs::read(out.join("ca.key.pem")).unwrap();

        tls_bootstrap(&["vbox-guest".to_string()], &out).unwrap();

        assert_eq!(
            std::fs::read(out.join("ca.pem")).unwrap(),
            ca_first,
            "CA cert must be reused across runs"
        );
        assert_eq!(
            std::fs::read(out.join("ca.key.pem")).unwrap(),
            ca_key_first,
            "CA key must be reused across runs"
        );
    }

    struct TempDir {
        path: PathBuf,
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
    fn tempdir_for_test() -> TempDir {
        let base = std::env::temp_dir();
        let suffix = format!(
            "vbox-client-ctl-tls-{}-{}",
            std::process::id(),
            unique_counter()
        );
        let path = base.join(suffix);
        std::fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }
    fn unique_counter() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static C: AtomicU64 = AtomicU64::new(0);
        C.fetch_add(1, Ordering::Relaxed)
    }
}
