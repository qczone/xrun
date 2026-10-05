use crate::{
    config::{Identity, ServerConfig, atomic_private_write, restrict_dir},
    protocol::*,
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, DnType,
    ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose,
};
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime},
};
use std::{
    io::Cursor,
    sync::{Arc, Mutex},
};
use time::{Duration, OffsetDateTime};

pub fn base32(bytes: &[u8]) -> String {
    const ABC: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    let (mut acc, mut bits) = (0u32, 0usize);
    let mut out = String::new();
    for &byte in bytes {
        acc = (acc << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ABC[((acc >> bits) & 31) as usize] as char)
        }
    }
    if bits > 0 {
        out.push(ABC[((acc << (5 - bits)) & 31) as usize] as char)
    }
    out
}
pub fn random_token() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS random source");
    base32(&bytes)
}
pub fn cert_der(pem: &str) -> Result<CertificateDer<'static>> {
    rustls_pemfile::certs(&mut Cursor::new(pem.as_bytes()))
        .next()
        .context("missing certificate")?
        .context("invalid certificate")
}
fn private_key(pem: &str) -> Result<PrivateKeyDer<'static>> {
    rustls_pemfile::private_key(&mut Cursor::new(pem.as_bytes()))?.context("missing private key")
}
pub fn pem(der: &[u8]) -> String {
    let encoded = STANDARD.encode(der);
    format!(
        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
        encoded
            .as_bytes()
            .chunks(64)
            .map(|s| std::str::from_utf8(s).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    )
}
pub fn ca_spki_pin(ca: &str) -> Result<String> {
    let der = cert_der(ca)?;
    let (_, cert) = x509_parser::parse_x509_certificate(&der)
        .map_err(|e| anyhow::anyhow!("invalid CA: {e}"))?;
    use sha2::{Digest, Sha256};
    Ok(base32(&Sha256::digest(
        cert.tbs_certificate.subject_pki.raw,
    )))
}
pub fn peer_identity(der: &[u8]) -> Result<(String, String)> {
    let (_, cert) = x509_parser::parse_x509_certificate(der)
        .map_err(|e| anyhow::anyhow!("invalid certificate: {e}"))?;
    let id = cert
        .subject()
        .iter_common_name()
        .next()
        .context("missing device ID")?
        .as_str()?
        .to_string();
    Ok((id, sha256(cert.tbs_certificate.subject_pki.raw)))
}
pub fn csr_key(csr: &[u8]) -> Result<String> {
    CertificateSigningRequestParams::from_der(&csr.into())?; // verifies proof of possession
    let (_, csr) = x509_parser::certification_request::X509CertificationRequest::from_der(csr)
        .map_err(|e| anyhow::anyhow!("INVALID_CSR: {e}"))?;
    Ok(sha256(csr.certification_request_info.subject_pki.raw))
}
use x509_parser::prelude::FromDer;
fn params(cn: &str, days: i64) -> CertificateParams {
    let mut p = CertificateParams::default();
    p.distinguished_name.push(DnType::CommonName, cn);
    p.not_before = OffsetDateTime::now_utc() - Duration::days(1);
    p.not_after = OffsetDateTime::now_utc() + Duration::days(days);
    p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    p
}
fn expiry(pem: &str) -> Result<OffsetDateTime> {
    let der = cert_der(pem)?;
    let (_, cert) = x509_parser::parse_x509_certificate(&der)
        .map_err(|e| anyhow::anyhow!("invalid certificate: {e}"))?;
    Ok(OffsetDateTime::from_unix_timestamp(
        cert.validity().not_after.timestamp(),
    )?)
}
pub struct ServerKeys {
    pub ca_pem: String,
    pub tls_config: Arc<rustls::ServerConfig>,
}
pub fn load_or_create_server(config: &ServerConfig) -> Result<ServerKeys> {
    let dir = &config.data_dir;
    std::fs::create_dir_all(dir)?;
    restrict_dir(dir)?;
    let names = ["ca.pem", "ca.key", "server.pem", "server.key"];
    let count = names.iter().filter(|n| dir.join(n).exists()).count();
    if count != 0 && count != 4 {
        bail!("TLS_IDENTITY_INCOMPLETE: refusing to replace deployment CA")
    }
    if count == 0 {
        let key = KeyPair::generate()?;
        let mut p = params("xrun deployment CA", 7300);
        p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        atomic_private_write(&dir.join("ca.pem"), p.self_signed(&key)?.pem().as_bytes())?;
        atomic_private_write(&dir.join("ca.key"), key.serialize_pem().as_bytes())?;
        atomic_private_write(
            &dir.join("server.key"),
            KeyPair::generate()?.serialize_pem().as_bytes(),
        )?;
    }
    let ca_pem = std::fs::read_to_string(dir.join("ca.pem"))?;
    let ca_key_pem = std::fs::read_to_string(dir.join("ca.key"))?;
    let ca_expiry = expiry(&ca_pem)?;
    if ca_expiry <= OffsetDateTime::now_utc() {
        bail!("CA_EXPIRED: deployment must be recreated")
    }
    let ca_key = KeyPair::from_pem(&ca_key_pem)?;
    let issuer = Issuer::from_ca_cert_pem(&ca_pem, &ca_key)?;
    let key_pem = std::fs::read_to_string(dir.join("server.key"))?;
    let key = KeyPair::from_pem(&key_pem)?;
    let mut hosts = config
        .urls()
        .iter()
        .map(|a| {
            url::Url::parse(a).and_then(|u| {
                u.host_str()
                    .map(str::to_string)
                    .ok_or(url::ParseError::EmptyHost)
            })
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    hosts.extend(["localhost".into(), "127.0.0.1".into()]);
    let mut p = CertificateParams::new(hosts)?;
    p.not_before = OffsetDateTime::now_utc() - Duration::days(1);
    p.not_after = (OffsetDateTime::now_utc() + Duration::days(3650)).min(ca_expiry);
    p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let leaf = p.signed_by(&key, &issuer)?.pem();
    atomic_private_write(&dir.join("server.pem"), leaf.as_bytes())?;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert_der(&ca_pem)?)?;
    let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
        .allow_unauthenticated()
        .build()?;
    let tls = rustls::ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(
            vec![cert_der(&leaf)?, cert_der(&ca_pem)?],
            private_key(&key_pem)?,
        )?;
    Ok(ServerKeys {
        ca_pem,
        tls_config: Arc::new(tls),
    })
}
pub fn new_device_request() -> Result<(String, Vec<u8>)> {
    let key = KeyPair::generate()?;
    let csr = CertificateParams::default().serialize_request(&key)?;
    Ok((key.serialize_pem(), csr.der().to_vec()))
}
pub fn renew_device_request(pem: &str) -> Result<Vec<u8>> {
    Ok(CertificateParams::default()
        .serialize_request(&KeyPair::from_pem(pem)?)?
        .der()
        .to_vec())
}
pub fn client_tls_config(id: &Identity) -> Result<Arc<rustls::ClientConfig>> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert_der(&id.ca_pem)?)?;
    Ok(Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_client_auth_cert(vec![cert_der(&id.cert_pem)?], private_key(&id.key_pem)?)?,
    ))
}
pub fn anonymous_tls_config(ca: &str) -> Result<Arc<rustls::ClientConfig>> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert_der(ca)?)?;
    Ok(Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ))
}
/// An empty relay CA selects public WebPKI roots. Peer TLS always uses the
/// separate network root, regardless of how the outer relay is hosted.
pub fn relay_tls_config(ca: &str) -> Result<Arc<rustls::ClientConfig>> {
    if !ca.is_empty() {
        return anonymous_tls_config(ca);
    }
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Ok(Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ))
}
pub fn http_client(ca: &str, id: Option<&Identity>) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .tls_certs_only([reqwest::Certificate::from_pem(ca.as_bytes())?])
        .no_proxy()
        .local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(30));
    if let Some(id) = id {
        builder = builder.identity(reqwest::Identity::from_pem(
            format!("{}\n{}", id.cert_pem, id.key_pem).as_bytes(),
        )?)
    }
    Ok(builder.build()?)
}
pub fn certificate_expiring(pem: &str, days: i64) -> Result<bool> {
    Ok(expiry(pem)? <= OffsetDateTime::now_utc() + Duration::days(days))
}

// The bootstrap verifier installs only the CA whose SPKI matches the invitation,
// then performs ordinary chain, time and hostname verification before TLS succeeds.
#[derive(Debug)]
struct PinVerifier {
    pin: String,
    ca: Arc<Mutex<Option<String>>>,
}
impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end: &CertificateDer<'_>,
        chain: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let result = (|| -> Result<_> {
            let ca = chain.last().context("server omitted deployment CA")?;
            let ca_pem = pem(ca);
            if ca_spki_pin(&ca_pem)? != self.pin {
                bail!("CA pin mismatch")
            }
            if expiry(&ca_pem)?.unix_timestamp() <= now.as_secs() as i64 {
                bail!("CA expired")
            }
            let mut roots = rustls::RootCertStore::empty();
            roots.add(ca.clone().into_owned())?;
            let verifier =
                rustls::client::WebPkiServerVerifier::builder(Arc::new(roots)).build()?;
            let verified =
                verifier.verify_server_cert(end, &chain[..chain.len() - 1], name, ocsp, now)?;
            *self.ca.lock().unwrap() = Some(ca_pem);
            Ok(verified)
        })();
        result.map_err(|e| rustls::Error::General(format!("{e:#}")))
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        s: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            m,
            c,
            s,
            &rustls::crypto::aws_lc_rs::default_provider().signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        s: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            m,
            c,
            s,
            &rustls::crypto::aws_lc_rs::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
pub async fn discover_ca(address: &str, pin: &str) -> Result<String> {
    let url = url::Url::parse(address)?;
    let host = url.host_str().context("missing host")?;
    let (config, ca) = pinned_client_config(pin);
    let tcp = crate::net::tcp(&url).await?;
    let tls = tokio_rustls::TlsConnector::from(config)
        .connect(ServerName::try_from(host.to_string())?, tcp)
        .await?;
    drop(tls);

    ca.lock().unwrap().clone().context("server omitted CA")
}
pub(crate) fn pinned_client_config(
    pin: &str,
) -> (Arc<rustls::ClientConfig>, Arc<Mutex<Option<String>>>) {
    let ca = Arc::new(Mutex::new(None));
    let verifier = PinVerifier {
        pin: pin.into(),
        ca: ca.clone(),
    };
    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    (Arc::new(config), ca)
}
pub(crate) fn peer_server_config(id: &Identity) -> Result<Arc<rustls::ServerConfig>> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert_der(&id.ca_pem)?)?;
    let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
        .allow_unauthenticated()
        .build()?;
    Ok(Arc::new(
        rustls::ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(
                vec![cert_der(&id.cert_pem)?, cert_der(&id.ca_pem)?],
                private_key(&id.key_pem)?,
            )?,
    ))
}
pub(crate) fn verify_member_certificate(cert: &str, root: &str, device: &str) -> Result<()> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert_der(root)?)?;
    let verifier = rustls::client::WebPkiServerVerifier::builder(Arc::new(roots)).build()?;
    verifier.verify_server_cert(
        &cert_der(cert)?,
        &[],
        &ServerName::try_from(crate::membership::device_name(device)?)?,
        &[],
        UnixTime::now(),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pinned_ca_still_enforces_hostname_and_signature_chain() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let cfg = ServerConfig {
            port: 9528,
            addresses: vec!["127.0.0.1:9528".into()],
            manual: true,
            no_detect: true,
            data_dir: temp.path().into(),
        };
        let keys = load_or_create_server(&cfg)?;
        let leaf = cert_der(&std::fs::read_to_string(temp.path().join("server.pem"))?)?;
        let root = cert_der(&keys.ca_pem)?;
        let verifier = PinVerifier {
            pin: ca_spki_pin(&keys.ca_pem)?,
            ca: Arc::new(Mutex::new(None)),
        };
        assert!(
            verifier
                .verify_server_cert(
                    &leaf,
                    std::slice::from_ref(&root),
                    &ServerName::try_from("localhost")?,
                    &[],
                    UnixTime::now()
                )
                .is_ok()
        );
        assert!(
            verifier
                .verify_server_cert(
                    &leaf,
                    &[root],
                    &ServerName::try_from("untrusted.invalid")?,
                    &[],
                    UnixTime::now()
                )
                .is_err()
        );
        Ok(())
    }
}
