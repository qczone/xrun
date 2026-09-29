use crate::{
    config::{Identity, ServerConfig, atomic_private_write, restrict_dir},
    protocol::{RenewRequest, RenewResponse, VERSION},
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, DnType,
    ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};
use std::{io::Cursor, sync::Arc};
use time::{Duration, OffsetDateTime};

pub struct ServerKeys {
    pub ca_pem: String,
    pub ca_pin: String,
    pub ca_key_pem: String,
    pub tls_config: Arc<rustls::ServerConfig>,
}

fn cert_der(pem: &str) -> Result<CertificateDer<'static>> {
    rustls_pemfile::certs(&mut Cursor::new(pem.as_bytes()))
        .next()
        .context("missing PEM certificate")?
        .context("invalid PEM certificate")
}

fn private_key(pem: &str) -> Result<PrivateKeyDer<'static>> {
    rustls_pemfile::private_key(&mut Cursor::new(pem.as_bytes()))?.context("missing private key")
}

pub fn certificate_fingerprint(pem: &str) -> Result<String> {
    Ok(hex::encode(Sha256::digest(cert_der(pem)?.as_ref())))
}

pub fn ca_spki_pin(ca_pem: &str) -> Result<String> {
    let der = cert_der(ca_pem)?;
    let (_, cert) = x509_parser::parse_x509_certificate(der.as_ref())
        .map_err(|e| anyhow::anyhow!("parse CA certificate: {e}"))?;
    Ok(hex::encode(Sha256::digest(
        cert.tbs_certificate.subject_pki.raw,
    )))
}

fn cert_params(common_name: &str, days: i64) -> CertificateParams {
    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, common_name);
    params.not_before = OffsetDateTime::now_utc() - Duration::days(1);
    params.not_after = OffsetDateTime::now_utc() + Duration::days(days);
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params
}

pub fn load_or_create_server(config: &ServerConfig) -> Result<ServerKeys> {
    let dir = &config.data_dir;
    std::fs::create_dir_all(dir)?;
    restrict_dir(dir)?;
    let files = ["ca.pem", "ca.key", "server.pem", "server.key"];
    let present = files.iter().filter(|name| dir.join(name).exists()).count();
    if present != 0 && present != files.len() {
        bail!(
            "incomplete TLS identity in {}; refusing to replace existing CA",
            dir.display()
        );
    }
    if present == 0 {
        let ca_key = KeyPair::generate()?;
        let mut ca_params = cert_params("xrun deployment CA", 3650);
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let ca_cert = ca_params.self_signed(&ca_key)?;
        let issuer = Issuer::from_params(&ca_params, &ca_key);
        let server_key = KeyPair::generate()?;
        let hostname = url::Url::parse(&config.public_url)?
            .host_str()
            .context("missing public host")?
            .to_string();
        let mut server_params = CertificateParams::new(vec![hostname])?;
        server_params.not_before = OffsetDateTime::now_utc() - Duration::days(1);
        server_params.not_after = OffsetDateTime::now_utc() + Duration::days(90);
        server_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let server_cert = server_params.signed_by(&server_key, &issuer)?;
        atomic_private_write(&dir.join("ca.pem"), ca_cert.pem().as_bytes())?;
        atomic_private_write(&dir.join("ca.key"), ca_key.serialize_pem().as_bytes())?;
        atomic_private_write(&dir.join("server.pem"), server_cert.pem().as_bytes())?;
        atomic_private_write(
            &dir.join("server.key"),
            server_key.serialize_pem().as_bytes(),
        )?;
    }
    let ca_pem = std::fs::read_to_string(dir.join("ca.pem"))?;
    let ca_key_pem = std::fs::read_to_string(dir.join("ca.key"))?;
    let server_key_pem = std::fs::read_to_string(dir.join("server.key"))?;
    // Reissue the leaf under the stable deployment CA on startup. This also picks up a
    // deliberately changed public IP without changing any paired device's trust anchor.
    if present == files.len() {
        let ca_key = KeyPair::from_pem(&ca_key_pem)?;
        let issuer = Issuer::from_ca_cert_pem(&ca_pem, &ca_key)?;
        let server_key = KeyPair::from_pem(&server_key_pem)?;
        let hostname = url::Url::parse(&config.public_url)?
            .host_str()
            .context("missing public host")?
            .to_string();
        let mut params = CertificateParams::new(vec![hostname])?;
        params.not_before = OffsetDateTime::now_utc() - Duration::days(1);
        params.not_after = OffsetDateTime::now_utc() + Duration::days(90);
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let cert = params.signed_by(&server_key, &issuer)?;
        atomic_private_write(&dir.join("server.pem"), cert.pem().as_bytes())?;
    }
    let server_pem = std::fs::read_to_string(dir.join("server.pem"))?;
    let ca_pin = ca_spki_pin(&ca_pem)?;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert_der(&ca_pem)?)?;
    let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
        .allow_unauthenticated()
        .build()?;
    let tls_config = rustls::ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(
            vec![cert_der(&server_pem)?, cert_der(&ca_pem)?],
            private_key(&server_key_pem)?,
        )?;
    Ok(ServerKeys {
        ca_pem,
        ca_pin,
        ca_key_pem,
        tls_config: Arc::new(tls_config),
    })
}

pub fn issue_device_certificate(
    ca_pem: &str,
    ca_key_pem: &str,
    csr: &[u8],
    device_id: &str,
) -> Result<String> {
    let ca_key = KeyPair::from_pem(ca_key_pem)?;
    let issuer = Issuer::from_ca_cert_pem(ca_pem, &ca_key)?;
    let mut request = CertificateSigningRequestParams::from_der(&csr.into())?;
    request.params.is_ca = IsCa::NoCa;
    request.params.subject_alt_names.clear();
    request.params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    request.params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    request.params.distinguished_name = rcgen::DistinguishedName::new();
    request
        .params
        .distinguished_name
        .push(DnType::CommonName, device_id);
    request.params.not_before = OffsetDateTime::now_utc() - Duration::days(1);
    request.params.not_after = OffsetDateTime::now_utc() + Duration::days(90);
    Ok(request.signed_by(&issuer)?.pem())
}

pub fn new_device_request() -> Result<(String, Vec<u8>)> {
    let key = KeyPair::generate()?;
    let csr = CertificateParams::default().serialize_request(&key)?;
    Ok((key.serialize_pem(), csr.der().as_ref().to_vec()))
}

pub fn renew_device_request(key_pem: &str) -> Result<Vec<u8>> {
    let key = KeyPair::from_pem(key_pem)?;
    Ok(CertificateParams::default()
        .serialize_request(&key)?
        .der()
        .as_ref()
        .to_vec())
}

pub fn client_tls_config(identity: &Identity) -> Result<Arc<rustls::ClientConfig>> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert_der(&identity.ca_pem)?)?;
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(
            vec![cert_der(&identity.cert_pem)?],
            private_key(&identity.key_pem)?,
        )?;
    Ok(Arc::new(config))
}

pub fn http_client(ca_pem: &str, identity: Option<&Identity>) -> Result<reqwest::Client> {
    let ca = reqwest::Certificate::from_pem(ca_pem.as_bytes())?;
    let mut builder = reqwest::Client::builder()
        .tls_certs_only([ca])
        .no_proxy()
        .timeout(std::time::Duration::from_secs(30));
    if let Some(identity) = identity {
        let pem = format!("{}\n{}", identity.cert_pem, identity.key_pem);
        builder = builder.identity(reqwest::Identity::from_pem(pem.as_bytes())?);
    }
    Ok(builder.build()?)
}

pub fn verify_ca_from_link(encoded_cert: &str, pin: &str) -> Result<String> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded_cert)?;
    let cert = CertificateDer::from(bytes);
    let (_, parsed) = x509_parser::parse_x509_certificate(cert.as_ref())
        .map_err(|e| anyhow::anyhow!("invalid CA in pairing link: {e}"))?;
    let actual = hex::encode(Sha256::digest(parsed.tbs_certificate.subject_pki.raw));
    if actual != pin {
        bail!("pairing link CA fingerprint mismatch");
    }
    // The CA is carried by the trusted pairing link, never fetched over an unverified channel.
    use base64::engine::general_purpose::STANDARD;
    let encoded = STANDARD.encode(cert.as_ref());
    let lines = encoded
        .as_bytes()
        .chunks(64)
        .map(|line| std::str::from_utf8(line).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        "-----BEGIN CERTIFICATE-----\n{lines}\n-----END CERTIFICATE-----\n"
    ))
}

pub fn ca_link_value(ca_pem: &str) -> Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(cert_der(ca_pem)?.as_ref()))
}

pub fn certificate_expiring(cert_pem: &str, within_days: i64) -> Result<bool> {
    let cert = cert_der(cert_pem)?;
    let (_, parsed) = x509_parser::parse_x509_certificate(cert.as_ref())
        .map_err(|e| anyhow::anyhow!("invalid certificate: {e}"))?;
    Ok(parsed.validity().not_after.timestamp()
        <= (OffsetDateTime::now_utc() + Duration::days(within_days)).unix_timestamp())
}

pub async fn renew_identity(identity: &mut Identity) -> Result<()> {
    let csr = renew_device_request(&identity.key_pem)?;
    let client = http_client(&identity.ca_pem, Some(identity))?;
    let response = client
        .post(format!("{}/devices/self/renew", identity.server_url))
        .header("x-xrun-version", VERSION)
        .json(&RenewRequest {
            csr_base64: base64::engine::general_purpose::STANDARD.encode(csr),
        })
        .send()
        .await?;
    if !response.status().is_success() {
        anyhow::bail!("certificate renewal failed: {}", response.text().await?);
    }
    let renewal: RenewResponse = response.json().await?;
    identity.cert_pem = renewal.cert_pem;
    identity.save()?;
    Ok(())
}
