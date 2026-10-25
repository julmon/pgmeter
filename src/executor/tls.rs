//! TLS connector for the PostgreSQL connections, built from libpq's
//! `sslmode` (RELEASE_REVIEW C4). rustls with the ring provider: nothing to
//! install at build time, and the system CA store is used for the verifying
//! modes.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use tokio_postgres_rustls::MakeRustlsConnect;

use super::error::Error;
use crate::args::SslMode;

/// Build the connector for `sslmode`. With `disable` the driver never
/// starts TLS, so the connector is unused; with `prefer` and `require` the
/// session is encrypted but the certificate is accepted as is, like libpq;
/// with `verify-ca` and `verify-full` the certificate must chain to a CA of
/// the system store and match the host name.
pub fn connector(sslmode: SslMode) -> Result<MakeRustlsConnect, Error> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::Other(format!("TLS setup failed: {e}")))?;

    let config = if sslmode.verifies_certificate() {
        let mut roots = RootCertStore::empty();
        let certs = rustls_native_certs::load_native_certs();
        for error in &certs.errors {
            eprintln!("WARNING: could not load a system CA certificate: {error}");
        }
        let (added, _ignored) = roots.add_parsable_certificates(certs.certs);
        if added == 0 {
            return Err(Error::Other(
                "sslmode verify-ca/verify-full: no CA certificate could be loaded from the system store"
                    .to_string(),
            ));
        }
        builder.with_root_certificates(roots).with_no_client_auth()
    } else {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCertificate { provider }))
            .with_no_client_auth()
    };
    Ok(MakeRustlsConnect::new(config))
}

/// Encrypts without authenticating the server: what libpq does for
/// `sslmode=require` (and `prefer`). Signatures are still checked so the
/// handshake itself is sound.
#[derive(Debug)]
struct AcceptAnyCertificate {
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for AcceptAnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
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
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connector_can_be_built_for_every_mode() {
        for mode in [SslMode::Disable, SslMode::Prefer, SslMode::Require] {
            connector(mode).unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        }
        // The verifying modes need a system CA store; on a machine without one
        // they must fail with a clear message rather than silently trusting.
        for mode in [SslMode::VerifyCa, SslMode::VerifyFull] {
            if let Err(e) = connector(mode) {
                assert!(e.to_string().contains("no CA certificate"), "{mode:?}: {e}");
            }
        }
    }
}
