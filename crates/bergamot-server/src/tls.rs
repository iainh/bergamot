use std::fs;
use std::path::{Path, PathBuf};

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose};

const CA_CERT_FILENAME: &str = "bergamot-ca.pem";
const CA_KEY_FILENAME: &str = "bergamot-ca-key.pem";
const SERVER_CERT_FILENAME: &str = "bergamot-server.pem";
const SERVER_KEY_FILENAME: &str = "bergamot-server-key.pem";

const CA_VALIDITY_DAYS: i64 = 3650;
const SERVER_VALIDITY_DAYS: i64 = 365;
const RENEWAL_THRESHOLD_DAYS: i64 = 30;

#[derive(Debug)]
pub struct CertPaths {
    pub cert: PathBuf,
    pub key: PathBuf,
}

pub fn ensure_certificates(cert_store: &Path) -> Result<CertPaths, Box<dyn std::error::Error>> {
    fs::create_dir_all(cert_store)?;

    let ca_cert_path = cert_store.join(CA_CERT_FILENAME);
    let ca_key_path = cert_store.join(CA_KEY_FILENAME);
    let server_cert_path = cert_store.join(SERVER_CERT_FILENAME);
    let server_key_path = cert_store.join(SERVER_KEY_FILENAME);

    let (ca_key, ca_cert_pem) = if ca_cert_path.exists() && ca_key_path.exists() {
        let key_pem = fs::read_to_string(&ca_key_path)?;
        let cert_pem = fs::read_to_string(&ca_cert_path)?;
        (KeyPair::from_pem(&key_pem)?, cert_pem)
    } else {
        tracing::info!("generating new CA certificate in {}", cert_store.display());
        let (key, cert_pem) = generate_ca()?;
        fs::write(&ca_key_path, key.serialize_pem())?;
        fs::write(&ca_cert_path, &cert_pem)?;
        restrict_permissions(&ca_key_path);
        tracing::info!(
            "CA certificate written to {} — import this into your browser/OS trust store to avoid warnings",
            ca_cert_path.display()
        );
        (key, cert_pem)
    };

    let needs_server_cert = if server_cert_path.exists() && server_key_path.exists() {
        server_cert_expiring_soon(&server_cert_path)?
    } else {
        true
    };

    if needs_server_cert {
        tracing::info!("generating new server certificate signed by local CA");
        let ca_issuer = Issuer::from_ca_cert_pem(&ca_cert_pem, ca_key)?;
        let (server_cert_pem, server_key_pem) = generate_server_cert(&ca_issuer, &ca_cert_pem)?;
        fs::write(&server_cert_path, &server_cert_pem)?;
        fs::write(&server_key_path, &server_key_pem)?;
        restrict_permissions(&server_key_path);
        tracing::info!(
            "server certificate written to {}",
            server_cert_path.display()
        );
    }

    Ok(CertPaths {
        cert: server_cert_path,
        key: server_key_path,
    })
}

fn generate_ca() -> Result<(KeyPair, String), Box<dyn std::error::Error>> {
    let key_pair = KeyPair::generate()?;

    let not_before = time::OffsetDateTime::now_utc();
    let not_after = not_before + time::Duration::days(CA_VALIDITY_DAYS);

    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, "Bergamot Local CA");
    params
        .distinguished_name
        .push(DnType::OrganizationName, "Bergamot");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params.not_before = not_before;
    params.not_after = not_after;

    let cert = params.self_signed(&key_pair)?;
    Ok((key_pair, cert.pem()))
}

fn generate_server_cert(
    ca_issuer: &Issuer<'_, impl rcgen::SigningKey>,
    ca_cert_pem: &str,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let server_key = KeyPair::generate()?;

    let not_before = time::OffsetDateTime::now_utc();
    let not_after = not_before + time::Duration::days(SERVER_VALIDITY_DAYS);

    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "localhost".to_string());

    let mut san_names = vec!["localhost".to_string()];
    if hostname != "localhost" {
        san_names.push(hostname);
    }

    let mut params = CertificateParams::new(san_names)?;
    params
        .distinguished_name
        .push(DnType::CommonName, "Bergamot Server");
    params
        .distinguished_name
        .push(DnType::OrganizationName, "Bergamot");
    params.not_before = not_before;
    params.not_after = not_after;
    params.use_authority_key_identifier_extension = true;

    params
        .subject_alt_names
        .push(rcgen::SanType::IpAddress(std::net::IpAddr::V4(
            std::net::Ipv4Addr::LOCALHOST,
        )));
    params
        .subject_alt_names
        .push(rcgen::SanType::IpAddress(std::net::IpAddr::V6(
            std::net::Ipv6Addr::LOCALHOST,
        )));

    let server_cert = params.signed_by(&server_key, ca_issuer)?;

    let mut chain = server_cert.pem();
    chain.push('\n');
    chain.push_str(ca_cert_pem);

    Ok((chain, server_key.serialize_pem()))
}

fn server_cert_expiring_soon(cert_path: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    let pem_data = fs::read_to_string(cert_path)?;
    let pem = match pem::parse(&pem_data) {
        Ok(p) => p,
        Err(_) => return Ok(true),
    };
    let der: rustls_pki_types::CertificateDer<'_> = pem.contents().into();
    let (_, x509) = match x509_parser::parse_x509_certificate(&der) {
        Ok(parsed) => parsed,
        Err(_) => return Ok(true),
    };
    let not_after = x509.validity().not_after.to_datetime();
    let now = time::OffsetDateTime::now_utc();
    let remaining = not_after - now;
    Ok(remaining.whole_days() < RENEWAL_THRESHOLD_DAYS)
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_ca_and_server_certs() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ensure_certificates(dir.path()).unwrap();
        assert!(paths.cert.exists());
        assert!(paths.key.exists());
        assert!(dir.path().join(CA_CERT_FILENAME).exists());
        assert!(dir.path().join(CA_KEY_FILENAME).exists());
    }

    #[test]
    fn reuses_existing_ca() {
        let dir = tempfile::tempdir().unwrap();
        let _ = ensure_certificates(dir.path()).unwrap();
        let ca_pem_1 = fs::read_to_string(dir.path().join(CA_CERT_FILENAME)).unwrap();

        let _ = ensure_certificates(dir.path()).unwrap();
        let ca_pem_2 = fs::read_to_string(dir.path().join(CA_CERT_FILENAME)).unwrap();

        assert_eq!(ca_pem_1, ca_pem_2);
    }
}
