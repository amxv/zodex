use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use anyhow::{Context, Result, bail};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};
use zodex::config::Config;

pub(super) fn ensure_reader_ready_for_start(config: &Config) -> Result<()> {
    let Some(app_id) = config.reader_app_id else {
        bail!("reader_app_id must be configured before start");
    };
    if app_id == 0 {
        bail!("reader_app_id must be non-zero");
    }

    let Some(installation_id) = config.reader_installation_id else {
        bail!("reader_installation_id must be configured before start");
    };
    if installation_id == 0 {
        bail!("reader_installation_id must be non-zero");
    }

    if config.reader_private_key_path.trim().is_empty() {
        bail!("reader_private_key_path must be configured");
    }
    if !Path::new(&config.reader_private_key_path).exists() {
        bail!(
            "reader private key file not found: {}",
            config.reader_private_key_path
        );
    }

    Ok(())
}

pub(super) fn ensure_tls_artifacts(config_path: &Path) -> Result<()> {
    let mut config = Config::load(Some(config_path))?;
    if tls_artifacts_exist(&config) {
        println!("tls-artifacts: ready");
        println!("tls-mode: {}", config.tls_mode);
        return Ok(());
    }

    let san_ip = select_tls_san_ip(&config.bind_host);
    generate_self_signed_certificate(&config, san_ip)?;
    config.tls_mode = "self_signed".to_string();
    config.save(config_path)?;

    println!("tls-artifacts: created");
    println!("tls-mode: {}", config.tls_mode);
    println!("tls-cert: {}", config.tls_cert_path);
    println!("tls-key: {}", config.tls_key_path);
    Ok(())
}

fn tls_artifacts_exist(config: &Config) -> bool {
    Path::new(&config.tls_cert_path).exists() && Path::new(&config.tls_key_path).exists()
}

fn select_tls_san_ip(bind_host: &str) -> std::net::IpAddr {
    if let Ok(ip) = bind_host.parse::<std::net::IpAddr>()
        && !ip.is_unspecified()
    {
        return ip;
    }
    std::net::IpAddr::from([127, 0, 0, 1])
}

fn generate_self_signed_certificate(config: &Config, ip: std::net::IpAddr) -> Result<()> {
    let mut params = CertificateParams::new(Vec::<String>::new())
        .context("failed to initialize self-signed cert parameters")?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, format!("zodex {ip}"));
    params.distinguished_name = dn;
    params.subject_alt_names = vec![SanType::IpAddress(ip)];

    let key_pair = KeyPair::generate().context("failed to generate TLS key pair")?;
    let certificate = params
        .self_signed(&key_pair)
        .context("failed to generate self-signed certificate")?;
    let cert_pem = certificate.pem();
    let key_pem = key_pair.serialize_pem();

    let cert_path = Path::new(&config.tls_cert_path);
    let key_path = Path::new(&config.tls_key_path);
    if let Some(parent) = cert_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    fs::write(cert_path, cert_pem)
        .with_context(|| format!("failed to write {}", cert_path.display()))?;
    fs::write(key_path, key_pem)
        .with_context(|| format!("failed to write {}", key_path.display()))?;

    #[cfg(unix)]
    {
        fs::set_permissions(cert_path, fs::Permissions::from_mode(0o644))
            .with_context(|| format!("failed to chmod {}", cert_path.display()))?;
        fs::set_permissions(key_path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to chmod {}", key_path.display()))?;
    }

    Ok(())
}
