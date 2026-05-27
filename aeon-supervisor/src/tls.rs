//! Generate a self-signed cert on first boot if one isn't present. PiKVM
//! does the same thing — the cert can be replaced with a Let's-Encrypt one
//! via the post-setup wizard (Tailscale also issues you a TLS cert if you
//! enable HTTPS in your tailnet).

use crate::api::Config;
use anyhow::{Context, Result};
use axum_server::tls_rustls::RustlsConfig;
use std::path::Path;

/// Make sure cert+key exist on disk (regenerate a self-signed pair if not),
/// then load them into a rustls config for axum-server.
///
/// Must be `async` because `RustlsConfig::from_pem_file` is async; calling
/// `runtime.block_on()` from inside `#[tokio::main]` deadlocks ("Cannot
/// start a runtime from within a runtime"). We're already inside a runtime
/// at the call site — just await directly.
pub async fn ensure_cert(cfg: &Config) -> Result<RustlsConfig> {
    let cert_path = &cfg.cert_path;
    let key_path = &cfg.key_path;

    if !cert_path.exists() || !key_path.exists() {
        tracing::info!("generating self-signed TLS certificate");
        let mut params = rcgen::CertificateParams::default();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "aeon-magick");
        params.subject_alt_names = vec![
            rcgen::SanType::DnsName(rcgen::Ia5String::try_from(
                "aeon-magick.local".to_string(),
            )?),
            rcgen::SanType::DnsName(rcgen::Ia5String::try_from("localhost".to_string())?),
        ];
        let key_pair = rcgen::KeyPair::generate()?;
        let cert = params.self_signed(&key_pair)?;
        write_pem(cert_path, &cert.pem())?;
        write_pem(key_path, &key_pair.serialize_pem())?;
    }

    RustlsConfig::from_pem_file(cert_path, key_path)
        .await
        .with_context(|| {
            format!(
                "loading TLS material from {} + {}",
                cert_path.display(),
                key_path.display()
            )
        })
}

fn write_pem(path: &Path, body: &str) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).ok();
    }
    std::fs::write(path, body).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if path.extension().map(|e| e == "pem").unwrap_or(false)
            && path.to_string_lossy().contains("key")
        {
            0o600
        } else {
            0o644
        };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

