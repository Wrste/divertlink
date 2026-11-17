//! TLS material loading, ALPN setup, certificate pinning and certificate generation.
//!
//! DivertLink uses `rustls` with the `ring` backend everywhere, so agents and
//! gateways are pure-Rust and easy to cross-compile statically. No OpenSSL is
//! required on any platform.

use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::config::{AgentTlsSettings, TlsServerSettings, TlsVersion};
use crate::error::{Error, Result};
use crate::protocol::{ALPN_INGRESS, ALPN_TUNNEL};

/// Maximum certificate chain size accepted when parsing PEM files.
const MAX_CERT_PEM_BYTES: usize = 4 * 1024 * 1024;

/// Install the process-wide default rustls crypto provider (`ring`).
///
/// Safe to call multiple times; returns silently if a provider was already set.
pub fn install_default_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    install_default_provider();
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Load a PEM certificate chain.
pub fn load_cert_chain(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let data = fs::read(path)
        .map_err(|e| Error::Tls(format!("cannot read certificate {}: {e}", path.display())))?;
    if data.len() > MAX_CERT_PEM_BYTES {
        return Err(Error::Tls(format!(
            "certificate file {} is unexpectedly large",
            path.display()
        )));
    }
    let mut reader = BufReader::new(&data[..]);
    let certs: std::result::Result<Vec<_>, _> = rustls_pemfile::certs(&mut reader).collect();
    let certs = certs
        .map_err(|e| Error::Tls(format!("invalid PEM certificate {}: {e}", path.display())))?;
    if certs.is_empty() {
        return Err(Error::Tls(format!(
            "no certificates found in {}",
            path.display()
        )));
    }
    Ok(certs)
}

/// Load a PEM private key (PKCS#8, PKCS#1 or SEC1).
pub fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>> {
    let data = fs::read(path)
        .map_err(|e| Error::Tls(format!("cannot read private key {}: {e}", path.display())))?;
    let mut reader = BufReader::new(&data[..]);
    rustls_pemfile::private_key(&mut reader)
        .map_err(|e| Error::Tls(format!("invalid PEM private key {}: {e}", path.display())))?
        .ok_or_else(|| Error::Tls(format!("no private key found in {}", path.display())))
}

/// Build the gateway's TLS acceptor configuration.
///
/// ALPN advertises `divertlink/1` plus `h2` / `http/1.1`, which lets one listener
/// serve both the agent tunnel and the public HTTPS ingress.
pub fn build_server_config(settings: &TlsServerSettings) -> Result<Arc<ServerConfig>> {
    let mut alpn = vec![ALPN_TUNNEL.to_vec()];
    alpn.extend(ALPN_INGRESS.iter().map(|p| p.to_vec()));
    build_server_config_with_alpn(settings, alpn)
}

/// Build a gateway TLS configuration with an explicit ALPN list.
///
/// Agents and public clients may share a listener (`divertlink/1` + `h2` +
/// `http/1.1`), but a dedicated ingress listener should advertise only HTTP ALPNs
/// so an agent cannot accidentally tunnel on the public port.
pub fn build_server_config_with_alpn(
    settings: &TlsServerSettings,
    alpn: Vec<Vec<u8>>,
) -> Result<Arc<ServerConfig>> {
    let certs = load_cert_chain(&settings.cert)?;
    let key = load_private_key(&settings.key)?;
    let builder = ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(settings.min_version.protocol_versions())
        .map_err(|e| Error::Tls(format!("unsupported TLS version configuration: {e}")))?;

    let mut config = if let Some(ca_path) = &settings.client_ca {
        let mut roots = RootCertStore::empty();
        let chain = load_cert_chain(ca_path)?;
        for cert in chain {
            roots
                .add(cert)
                .map_err(|e| Error::Tls(format!("invalid client CA certificate: {e}")))?;
        }
        let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
            .build()
            .map_err(|e| Error::Tls(format!("cannot build mTLS client verifier: {e}")))?;
        builder
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, key)
            .map_err(|e| Error::Tls(format!("invalid server certificate/key pair: {e}")))?
    } else {
        builder
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| Error::Tls(format!("invalid server certificate/key pair: {e}")))?
    };

    config.alpn_protocols = alpn;
    Ok(Arc::new(config))
}

/// Build the agent's TLS client configuration for the tunnel connection.
pub fn build_agent_client_config(settings: &AgentTlsSettings) -> Result<Arc<ClientConfig>> {
    let mut roots = RootCertStore::empty();
    match &settings.ca {
        Some(ca_path) => {
            for cert in load_cert_chain(ca_path)? {
                roots
                    .add(cert)
                    .map_err(|e| Error::Tls(format!("invalid CA certificate: {e}")))?;
            }
        }
        None => {
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        }
    }

    let provider = provider();
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(TlsVersion::V1_3.protocol_versions())
        .map_err(|e| Error::Tls(format!("TLS configuration error: {e}")))?;

    let verifier: Arc<dyn ServerCertVerifier> = if settings.insecure {
        Arc::new(NoVerifier {
            provider: provider.clone(),
        })
    } else if let Some(pin_text) = &settings.pin {
        let pin = decode_pin(pin_text)?;
        let webpki = rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            provider.clone(),
        )
        .build()
        .map_err(|e| Error::Tls(format!("cannot build certificate verifier: {e}")))?;
        Arc::new(PinnedVerifier { inner: webpki, pin })
    } else {
        let webpki = rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            provider.clone(),
        )
        .build()
        .map_err(|e| Error::Tls(format!("cannot build certificate verifier: {e}")))?;
        // WebPkiServerVerifier implements the danger trait directly; wrap it so
        // the type is erased into `Arc<dyn ServerCertVerifier>`.
        Arc::new(PassthroughVerifier { inner: webpki })
    };

    let builder = builder
        .dangerous()
        .with_custom_certificate_verifier(verifier);
    let mut config = match (&settings.client_cert, &settings.client_key) {
        (Some(cert), Some(key)) => builder
            .with_client_auth_cert(load_cert_chain(cert)?, load_private_key(key)?)
            .map_err(|e| Error::Tls(format!("invalid agent client certificate: {e}")))?,
        _ => builder.with_no_client_auth(),
    };
    config.alpn_protocols = vec![ALPN_TUNNEL.to_vec()];
    Ok(Arc::new(config))
}

/// Build an outgoing HTTPS client configuration (for local targets and static upstreams).
pub fn build_https_client_config(extra_ca: Option<&Path>) -> Result<Arc<ClientConfig>> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(path) = extra_ca {
        for cert in load_cert_chain(path)? {
            roots
                .add(cert)
                .map_err(|e| Error::Tls(format!("invalid upstream CA certificate: {e}")))?;
        }
    }
    // NOTE: `hyper-rustls` rewrites `alpn_protocols` itself and panics if they
    // are pre-set, so this config must leave them empty.
    let config = ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(TlsVersion::V1_2.protocol_versions())
        .map_err(|e| Error::Tls(format!("TLS configuration error: {e}")))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Turn a host name or IP literal into a rustls `ServerName`.
pub fn server_name(host: &str) -> Result<ServerName<'static>> {
    ServerName::try_from(host.to_string())
        .map_err(|_| Error::Tls(format!("invalid TLS server name `{host}`")))
}

/// Compute the SHA-256 pin over the certificate's SubjectPublicKeyInfo.
///
/// Pinning the SPKI instead of the whole certificate allows certificate renewal
/// with the same key pair to keep the pin valid.
pub fn spki_pin(cert_der: &[u8]) -> Result<String> {
    let (_, cert) = x509_parser::parse_x509_certificate(cert_der)
        .map_err(|e| Error::Tls(format!("cannot parse certificate for pinning: {e}")))?;
    let spki = cert.tbs_certificate.subject_pki.raw;
    let digest = Sha256::digest(spki);
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest))
}

/// Verify a peer's certificate chain against an expected SPKI pin.
///
/// Accepts base64 (standard or URL-safe, with or without padding) or hex pins,
/// optionally prefixed with `sha256/` or `sha256//` for compatibility with
/// common certificate-pinning notations.
pub fn verify_peer_pin(
    peer_certs: Option<&[CertificateDer<'static>]>,
    expected: &str,
) -> Result<()> {
    let expected = decode_pin(expected)?;
    let chain =
        peer_certs.ok_or_else(|| Error::Pin("peer did not present a certificate".into()))?;
    let leaf = chain
        .first()
        .ok_or_else(|| Error::Pin("empty peer certificate chain".into()))?;
    let actual = pin_digest(leaf.as_ref())?;
    if actual.ct_eq(&expected).into() {
        Ok(())
    } else {
        Err(Error::Pin(
            "server certificate SPKI does not match the configured pin".into(),
        ))
    }
}

fn pin_digest(cert_der: &[u8]) -> Result<[u8; 32]> {
    let (_, cert) = x509_parser::parse_x509_certificate(cert_der)
        .map_err(|e| Error::Tls(format!("cannot parse peer certificate: {e}")))?;
    Ok(Sha256::digest(cert.tbs_certificate.subject_pki.raw).into())
}

fn decode_pin(expected: &str) -> Result<[u8; 32]> {
    let mut raw = expected.trim().to_string();
    if let Some(rest) = raw.strip_prefix("sha256//") {
        raw = rest.to_string();
    } else if let Some(rest) = raw.strip_prefix("sha256/") {
        raw = rest.to_string();
    }
    raw.retain(|c| !c.is_whitespace());
    if raw.len() == 64 && raw.chars().all(|c| c.is_ascii_hexdigit()) {
        let bytes = (0..32)
            .map(|i| u8::from_str_radix(&raw[i * 2..i * 2 + 2], 16))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Pin(format!("invalid hex pin: {e}")))?;
        return bytes
            .try_into()
            .map_err(|_| Error::Pin("invalid pin length".into()));
    }
    for engine in [
        base64::engine::general_purpose::STANDARD,
        base64::engine::general_purpose::STANDARD_NO_PAD,
        base64::engine::general_purpose::URL_SAFE,
        base64::engine::general_purpose::URL_SAFE_NO_PAD,
    ] {
        if let Ok(bytes) = engine.decode(raw.as_bytes()) {
            if bytes.len() == 32 {
                return bytes
                    .try_into()
                    .map_err(|_| Error::Pin("invalid pin length".into()));
            }
        }
    }
    Err(Error::Pin(
        "pin must be a 32-byte SHA-256 digest encoded as hex or base64".into(),
    ))
}

/// A rustls verifier that accepts any certificate. Debugging only.
#[derive(Debug)]
struct NoVerifier {
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Delegating verifier used to erase [`WebPkiServerVerifier`] into a trait object.
#[derive(Debug)]
struct PassthroughVerifier {
    inner: Arc<rustls::client::WebPkiServerVerifier>,
}

impl ServerCertVerifier for PassthroughVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        self.inner
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// A rustls verifier that performs normal WebPKI validation and then additionally
/// enforces an SPKI SHA-256 pin on the leaf certificate.
#[derive(Debug)]
struct PinnedVerifier {
    inner: Arc<rustls::client::WebPkiServerVerifier>,
    pin: [u8; 32],
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let verified = self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        )?;
        let actual = pin_digest(end_entity.as_ref())
            .map_err(|_| rustls::Error::General("cannot parse certificate for pinning".into()))?;
        if actual.ct_eq(&self.pin).into() {
            Ok(verified)
        } else {
            Err(rustls::Error::General(
                "server certificate does not match the configured SPKI pin".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

// ---------------------------------------------------------------------------
// Certificate generation
// ---------------------------------------------------------------------------

/// Certificate generation options.
#[derive(Debug, Clone)]
pub struct GenCertsOptions {
    /// Output directory.
    pub dir: PathBuf,
    /// DNS name / IP the server certificate must be valid for.
    pub domain: String,
    /// Generate a client certificate for mTLS as well.
    pub client_cert: bool,
    /// Generate a single self-signed server certificate instead of a CA + leaf pair.
    pub self_signed: bool,
    /// Overwrite existing files.
    pub force: bool,
}

/// Result of certificate generation, with everything needed to wire up configs.
#[derive(Debug, Clone)]
pub struct CertReport {
    /// CA certificate (self-signed mode: the server certificate itself).
    pub ca_cert: PathBuf,
    /// CA private key (self-signed mode: `None`).
    pub ca_key: Option<PathBuf>,
    /// Server certificate chain.
    pub server_cert: PathBuf,
    /// Server private key.
    pub server_key: PathBuf,
    /// Client certificate (mTLS only).
    pub client_cert: Option<PathBuf>,
    /// Client private key (mTLS only).
    pub client_key: Option<PathBuf>,
    /// SPKI SHA-256 pin of the server certificate (base64url, no padding).
    pub server_pin: String,
    /// Whether any file was skipped because it already existed.
    pub skipped: bool,
}

/// Generate a CA, server certificate and (optionally) a client certificate.
pub fn generate_certs(opts: &GenCertsOptions) -> Result<CertReport> {
    use rcgen::{
        BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
        KeyUsagePurpose,
    };

    let dir = &opts.dir;
    fs::create_dir_all(dir)
        .map_err(|e| Error::Tls(format!("cannot create {}: {e}", dir.display())))?;
    let cert_path = dir.join("server.crt");
    let key_path = dir.join("server.key");
    if !opts.force && (cert_path.exists() || key_path.exists()) {
        return Err(Error::Tls(format!(
            "{} already exists; pass --force to overwrite certificates",
            cert_path.display()
        )));
    }

    let mut sans = vec![opts.domain.trim().to_string()];
    for extra in ["localhost", "127.0.0.1"] {
        if !sans.iter().any(|s| s == extra) {
            sans.push(extra.to_string());
        }
    }
    if opts.domain.trim().is_empty() {
        return Err(Error::Tls("certificate domain must not be empty".into()));
    }

    let server_key =
        KeyPair::generate().map_err(|e| Error::Tls(format!("cannot generate server key: {e}")))?;
    let mut server_params = CertificateParams::new(sans)
        .map_err(|e| Error::Tls(format!("invalid certificate domain: {e}")))?;
    server_params
        .distinguished_name
        .push(DnType::CommonName, "divertlink-server");
    server_params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    server_params.is_ca = IsCa::NoCa;

    // Self-signed mode has no CA; the server certificate is its own trust anchor.
    // CA mode generates a long-lived CA and a leaf signed by it. The same issuer
    // is reused to sign the optional mTLS client certificate below.
    let (ca_cert_path, ca_key_path, ca_issuer, self_signed_cert) = if opts.self_signed {
        let cert = server_params
            .self_signed(&server_key)
            .map_err(|e| Error::Tls(format!("cannot self-sign server certificate: {e}")))?;
        (cert_path.clone(), None, None, Some(cert))
    } else {
        let ca_key =
            KeyPair::generate().map_err(|e| Error::Tls(format!("cannot generate CA key: {e}")))?;
        let mut ca_params = CertificateParams::default();
        ca_params.distinguished_name.push(
            DnType::CommonName,
            format!("DivertLink CA ({})", opts.domain),
        );
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let ca_cert = ca_params
            .self_signed(&ca_key)
            .map_err(|e| Error::Tls(format!("cannot create CA certificate: {e}")))?;
        let ca_key_pem = ca_key.serialize_pem();
        write_cert(&dir.join("ca.crt"), ca_cert.der(), opts.force)?;
        write_private_key(&dir.join("ca.key"), &ca_key_pem, opts.force)?;
        let issuer: rcgen::Issuer<'static, KeyPair> = rcgen::Issuer::new(ca_params, ca_key);
        (
            dir.join("ca.crt"),
            Some(dir.join("ca.key")),
            Some(issuer),
            None,
        )
    };

    let server_cert = match &ca_issuer {
        Some(issuer) => server_params
            .signed_by(&server_key, issuer)
            .map_err(|e| Error::Tls(format!("cannot sign server certificate: {e}")))?,
        None => self_signed_cert.expect("self-signed certificate"),
    };
    let server_der = server_cert.der().to_vec();
    write_cert(&cert_path, &server_der, opts.force)?;
    write_private_key(&key_path, &server_key.serialize_pem(), opts.force)?;
    let server_pin = spki_pin(&server_der)?;

    let mut client_cert_path = None;
    let mut client_key_path = None;
    if opts.client_cert {
        let issuer = ca_issuer.as_ref().ok_or_else(|| {
            Error::Tls(
                "client certificates require a CA; do not combine --self-signed with --client"
                    .into(),
            )
        })?;
        let client_key = KeyPair::generate()
            .map_err(|e| Error::Tls(format!("cannot generate client key: {e}")))?;
        let mut client_params = CertificateParams::default();
        client_params
            .distinguished_name
            .push(DnType::CommonName, "divertlink-agent");
        client_params.is_ca = IsCa::NoCa;
        client_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        client_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let client_cert = client_params
            .signed_by(&client_key, issuer)
            .map_err(|e| Error::Tls(format!("cannot sign client certificate: {e}")))?;
        let client_cert_file = dir.join("client.crt");
        let client_key_file = dir.join("client.key");
        write_cert(&client_cert_file, client_cert.der(), opts.force)?;
        write_private_key(&client_key_file, &client_key.serialize_pem(), opts.force)?;
        client_cert_path = Some(client_cert_file);
        client_key_path = Some(client_key_file);
    }

    Ok(CertReport {
        ca_cert: ca_cert_path,
        ca_key: ca_key_path,
        server_cert: cert_path,
        server_key: key_path,
        client_cert: client_cert_path,
        client_key: client_key_path,
        server_pin,
        skipped: false,
    })
}

fn write_cert(path: &Path, der: &[u8], force: bool) -> Result<()> {
    if path.exists() && !force {
        return Err(Error::Tls(format!("{} already exists", path.display())));
    }
    let pem = pem_encode_cert(der);
    fs::write(path, pem).map_err(|e| Error::Tls(format!("cannot write {}: {e}", path.display())))
}

fn write_private_key(path: &Path, pem: &str, force: bool) -> Result<()> {
    if path.exists() && !force {
        return Err(Error::Tls(format!("{} already exists", path.display())));
    }
    fs::write(path, pem)
        .map_err(|e| Error::Tls(format!("cannot write {}: {e}", path.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn pem_encode_cert(der: &[u8]) -> String {
    use base64::engine::general_purpose::STANDARD;
    let b64 = STANDARD.encode(der);
    let mut out = String::with_capacity(b64.len() + 64);
    out.push_str("-----BEGIN CERTIFICATE-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        out.push('\n');
    }
    out.push_str("-----END CERTIFICATE-----\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn opts(dir: &Path) -> GenCertsOptions {
        GenCertsOptions {
            dir: dir.to_path_buf(),
            domain: "localhost".to_string(),
            client_cert: false,
            self_signed: false,
            force: false,
        }
    }

    #[test]
    fn generates_ca_and_leaf() {
        let tmp = TempDir::new().unwrap();
        let report = generate_certs(&opts(tmp.path())).unwrap();
        assert!(report.ca_cert.exists());
        assert!(report.server_cert.exists());
        assert!(report.server_key.exists());
        assert!(!report.server_pin.is_empty());
        let certs = load_cert_chain(&report.server_cert).unwrap();
        assert_eq!(certs.len(), 1);
        assert_eq!(spki_pin(certs[0].as_ref()).unwrap(), report.server_pin);
        let _ = load_private_key(&report.server_key).unwrap();
        // A CA + leaf can be loaded as a server config.
        let settings = TlsServerSettings {
            cert: report.server_cert.clone(),
            key: report.server_key.clone(),
            client_ca: Some(report.ca_cert.clone()),
            min_version: TlsVersion::V1_3,
        };
        // mTLS config requires client CA to be a valid CA; generated CA is.
        let _config = build_server_config(&settings).unwrap();
    }

    #[test]
    fn generates_self_signed() {
        let tmp = TempDir::new().unwrap();
        let mut o = opts(tmp.path());
        o.self_signed = true;
        let report = generate_certs(&o).unwrap();
        assert!(report.server_cert.exists());
        assert!(report.ca_key.is_none());
        let settings = TlsServerSettings {
            cert: report.server_cert.clone(),
            key: report.server_key.clone(),
            client_ca: None,
            min_version: TlsVersion::V1_3,
        };
        build_server_config(&settings).unwrap();
        // The self-signed cert itself can be used as the agent's trust anchor.
        let roots = load_cert_chain(&report.ca_cert).unwrap();
        assert_eq!(roots.len(), 1);
    }

    #[test]
    fn generates_client_cert() {
        let tmp = TempDir::new().unwrap();
        let mut o = opts(tmp.path());
        o.client_cert = true;
        let report = generate_certs(&o).unwrap();
        assert!(report.client_cert.unwrap().exists());
        assert!(report.client_key.unwrap().exists());
    }

    #[test]
    fn refuses_overwrite_without_force() {
        let tmp = TempDir::new().unwrap();
        let o = opts(tmp.path());
        generate_certs(&o).unwrap();
        assert!(generate_certs(&o).is_err());
        let mut forced = o.clone();
        forced.force = true;
        generate_certs(&forced).unwrap();
    }

    #[test]
    fn pin_verification() {
        let tmp = TempDir::new().unwrap();
        let report = generate_certs(&opts(tmp.path())).unwrap();
        let certs = load_cert_chain(&report.server_cert).unwrap();
        verify_peer_pin(Some(&certs), &report.server_pin).unwrap();
        // hex form
        let raw = decode_pin(&report.server_pin).unwrap();
        let hex = crate::util::hex(&raw);
        verify_peer_pin(Some(&certs), &hex).unwrap();
        // wrong pin
        let mut wrong = report.server_pin.clone();
        wrong.push('A');
        assert!(verify_peer_pin(Some(&certs), &wrong).is_err());
        assert!(verify_peer_pin(None, &report.server_pin).is_err());
    }

    #[test]
    fn invalid_pin_formats_rejected() {
        assert!(decode_pin("not-a-pin").is_err());
        assert!(decode_pin(&"aa".repeat(16)).is_err());
        assert!(decode_pin(&"ab".repeat(32)).is_ok());
    }

    #[tokio::test]
    async fn mtls_handshake_uses_client_certificate() {
        let tmp = TempDir::new().unwrap();
        let report = generate_certs(&GenCertsOptions {
            dir: tmp.path().to_path_buf(),
            domain: "localhost".to_string(),
            client_cert: true,
            self_signed: false,
            force: false,
        })
        .unwrap();
        let server_cfg = build_server_config(&TlsServerSettings {
            cert: report.server_cert.clone(),
            key: report.server_key.clone(),
            client_ca: Some(report.ca_cert.clone()),
            min_version: TlsVersion::V1_3,
        })
        .unwrap();
        let agent_cfg = build_agent_client_config(&AgentTlsSettings {
            ca: Some(report.ca_cert.clone()),
            pin: None,
            servername: Some("localhost".into()),
            client_cert: report.client_cert.clone(),
            client_key: report.client_key.clone(),
            insecure: false,
        })
        .unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(server_cfg);
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            acceptor.accept(tcp).await.map(|_| ())
        });
        let connector = tokio_rustls::TlsConnector::from(agent_cfg);
        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let client = connector
            .connect(server_name("localhost").unwrap(), tcp)
            .await;
        assert!(
            client.is_ok(),
            "client handshake failed: {:?}",
            client.err()
        );
        let server = server.await.unwrap();
        assert!(
            server.is_ok(),
            "server handshake failed: {:?}",
            server.err()
        );
    }

    #[test]
    fn client_config_builds_with_ca() {
        let tmp = TempDir::new().unwrap();
        let report = generate_certs(&opts(tmp.path())).unwrap();
        let settings = AgentTlsSettings {
            ca: Some(report.ca_cert.clone()),
            pin: Some(report.server_pin.clone()),
            servername: Some("localhost".into()),
            client_cert: None,
            client_key: None,
            insecure: false,
        };
        build_agent_client_config(&settings).unwrap();
        let insecure = AgentTlsSettings {
            insecure: true,
            ..Default::default()
        };
        build_agent_client_config(&insecure).unwrap();
    }
}
