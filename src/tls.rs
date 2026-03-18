use async_trait::async_trait;
use openssl::pkey::PKey;
use openssl::ssl::NameType;
use openssl::x509::X509;
use pingora::listeners::TlsAccept;
use pingora::listeners::tls::TlsSettings;
use pingora::tls::ext;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::error;

struct TlsCertificate {
    cert: X509,
    key: PKey<openssl::pkey::Private>,
    chain: Vec<X509>,
}

/// Dynamic certificate store that can be updated at runtime.
/// Shared via Arc so Pingora's TlsAccept callback and ProxyManager can both access it.
pub struct DynamicCertStore {
    certs: Arc<RwLock<HashMap<String, TlsCertificate>>>,
    default_cert: Arc<RwLock<Option<TlsCertificate>>>,
}

impl DynamicCertStore {
    pub fn new() -> Self {
        Self {
            certs: Arc::new(RwLock::new(HashMap::new())),
            default_cert: Arc::new(RwLock::new(None)),
        }
    }

    /// Add a certificate for a specific hostname (or "default" for the fallback cert).
    pub async fn add_cert(
        &self,
        hostname: &str,
        cert_path: &str,
        key_path: &str,
    ) -> Result<(), String> {
        let cert_pem = std::fs::read(cert_path)
            .map_err(|e| format!("failed to read cert {cert_path}: {e}"))?;
        let key_pem =
            std::fs::read(key_path).map_err(|e| format!("failed to read key {key_path}: {e}"))?;

        let cert = X509::from_pem(&cert_pem).map_err(|e| format!("failed to parse cert: {e}"))?;
        let key = PKey::private_key_from_pem(&key_pem)
            .map_err(|e| format!("failed to parse key: {e}"))?;

        let tls_cert = TlsCertificate {
            cert,
            key,
            chain: vec![],
        };

        if hostname == "default" {
            *self.default_cert.write().await = Some(tls_cert);
        } else {
            self.certs
                .write()
                .await
                .insert(hostname.to_string(), tls_cert);
        }

        Ok(())
    }

    /// Add a certificate from PEM bytes directly (used by ACME).
    pub async fn add_cert_from_pem(
        &self,
        hostname: &str,
        cert_pem: &[u8],
        key_pem: &[u8],
    ) -> Result<(), String> {
        let certs = X509::stack_from_pem(cert_pem)
            .map_err(|e| format!("failed to parse cert chain: {e}"))?;
        let key =
            PKey::private_key_from_pem(key_pem).map_err(|e| format!("failed to parse key: {e}"))?;

        let cert = certs
            .first()
            .ok_or_else(|| "empty certificate chain".to_string())?
            .clone();
        let chain = certs.into_iter().skip(1).collect();

        let tls_cert = TlsCertificate { cert, key, chain };

        if hostname == "default" {
            *self.default_cert.write().await = Some(tls_cert);
        } else {
            self.certs
                .write()
                .await
                .insert(hostname.to_string(), tls_cert);
        }

        Ok(())
    }

    /// Remove a certificate for a hostname.
    #[allow(dead_code)]
    pub async fn remove_cert(&self, hostname: &str) {
        if hostname == "default" {
            *self.default_cert.write().await = None;
        } else {
            self.certs.write().await.remove(hostname);
        }
    }

    /// Create TlsSettings using this dynamic store for Pingora.
    pub fn to_tls_settings(&self) -> Result<TlsSettings, String> {
        let callback = CertCallback {
            certs: Arc::clone(&self.certs),
            default_cert: Arc::clone(&self.default_cert),
        };
        TlsSettings::with_callbacks(Box::new(callback))
            .map_err(|e| format!("failed to create TLS settings: {e}"))
    }
}

struct CertCallback {
    certs: Arc<RwLock<HashMap<String, TlsCertificate>>>,
    default_cert: Arc<RwLock<Option<TlsCertificate>>>,
}

#[async_trait]
impl TlsAccept for CertCallback {
    async fn certificate_callback(&self, ssl: &mut pingora::protocols::tls::TlsRef) {
        let server_name = ssl.servername(NameType::HOST_NAME).unwrap_or("localhost");

        let certs = self.certs.read().await;
        let default = self.default_cert.read().await;

        let cert_info = if let Some(cert) = certs.get(server_name) {
            cert
        } else if let Some(cert) = default.as_ref() {
            cert
        } else {
            error!(sni = %server_name, "no certificate found");
            return;
        };

        if let Err(e) = ext::ssl_use_certificate(ssl, &cert_info.cert) {
            error!("failed to use certificate: {e}");
        }
        if let Err(e) = ext::ssl_use_private_key(ssl, &cert_info.key) {
            error!("failed to use private key: {e}");
        }
        for chain_cert in &cert_info.chain {
            if let Err(e) = ext::ssl_add_chain_cert(ssl, chain_cert) {
                error!("failed to add chain cert: {e}");
            }
        }
    }
}
