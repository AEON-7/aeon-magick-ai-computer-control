//! Generate a self-signed cert on first boot if one isn't present. PiKVM
//! does the same thing — the cert can be replaced with a Let's-Encrypt one
//! via the post-setup wizard (Tailscale also issues you a TLS cert if you
//! enable HTTPS in your tailnet).

use crate::api::Config;
use anyhow::{Context, Result};
use axum_server::tls_rustls::RustlsConfig;
use std::path::Path;

pub fn ensure_cert(cfg: &Config) -> Result<RustlsConfig> {
    let cert_path = &cfg.cert_path;
    let key_path = &cfg.key_path;

    if !cert_path.exists() || !key_path.exists() {
        tracing::info!("generating self-signed TLS certificate");
        let mut params = rcgen::CertificateParams::default();
        params.distinguished_name.push(rcgen::DnType::CommonName, "acursed-kvm");
        params.subject_alt_names = vec![
            rcgen::SanType::DnsName(rcgen::Ia5String::try_from("acursed.local".to_string())?),
            rcgen::SanType::DnsName(rcgen::Ia5String::try_from("localhost".to_string())?),
        ];
        let key_pair = rcgen::KeyPair::generate()?;
        let cert = params.self_signed(&key_pair)?;
        write_pem(cert_path, &cert.pem())?;
        write_pem(key_path, &key_pair.serialize_pem())?;
    }

    let runtime = tokio::runtime::Handle::current();
    let cfg = futures_rustls_helper(cert_path, key_path);
    runtime.block_on(cfg)
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

async fn futures_rustls_helper(cert: &Path, key: &Path) -> Result<RustlsConfig> {
    Ok(RustlsConfig::from_pem_file(cert, key).await?)
}
