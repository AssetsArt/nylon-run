use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, LetsEncrypt,
    NewAccount, NewOrder, OrderStatus,
};
use rcgen::{CertificateParams, DistinguishedName, KeyPair};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::tls::DynamicCertStore;

const CERTS_DIR: &str = "/var/run/nyrun/certs";
const RENEW_BEFORE_DAYS: i64 = 30;

/// In-memory store for ACME HTTP-01 challenge tokens.
/// Key: token, Value: key_authorization
#[derive(Clone, Default)]
pub struct ChallengeStore {
    tokens: Arc<RwLock<HashMap<String, String>>>,
}

impl ChallengeStore {
    pub async fn set(&self, token: String, key_auth: String) {
        self.tokens.write().await.insert(token, key_auth);
    }

    pub async fn get(&self, token: &str) -> Option<String> {
        self.tokens.read().await.get(token).cloned()
    }

    pub async fn remove(&self, token: &str) {
        self.tokens.write().await.remove(token);
    }
}

/// Issue a certificate for the given hostname via Let's Encrypt ACME (HTTP-01).
pub async fn issue_cert(
    email: &str,
    hostname: &str,
    challenge_store: &ChallengeStore,
    cert_store: &DynamicCertStore,
) -> Result<(), String> {
    let cert_dir = PathBuf::from(CERTS_DIR).join(hostname);
    let cert_path = cert_dir.join("cert.pem");
    let key_path = cert_dir.join("key.pem");
    let account_path = PathBuf::from(CERTS_DIR).join("account.json");

    // Check if existing cert is still valid
    if cert_path.exists()
        && key_path.exists()
        && let Ok(cert_pem) = std::fs::read(&cert_path)
        && let Ok(cert) = openssl::x509::X509::from_pem(&cert_pem)
    {
        let not_after = cert.not_after();
        let now = openssl::asn1::Asn1Time::days_from_now(0)
            .map_err(|e| format!("time error: {e}"))?;
        let renew_at = openssl::asn1::Asn1Time::days_from_now(RENEW_BEFORE_DAYS as u32)
            .map_err(|e| format!("time error: {e}"))?;

        if not_after > renew_at {
            // Cert is still valid, just load it
            info!(hostname, "existing ACME cert still valid, loading");
            cert_store
                .add_cert(hostname, cert_path.to_str().unwrap(), key_path.to_str().unwrap())
                .await?;
            return Ok(());
        }
        if not_after > now {
            info!(hostname, "ACME cert needs renewal soon");
        }
    }

    info!(hostname, email, "requesting ACME certificate");

    // Load or create ACME account
    let account = if account_path.exists() {
        let creds_json = std::fs::read_to_string(&account_path)
            .map_err(|e| format!("failed to read account: {e}"))?;
        let creds: AccountCredentials = serde_json::from_str(&creds_json)
            .map_err(|e| format!("failed to parse account: {e}"))?;
        Account::from_credentials(creds)
            .await
            .map_err(|e| format!("failed to load account: {e}"))?
    } else {
        let (account, creds) = Account::create(
            &NewAccount {
                contact: &[&format!("mailto:{email}")],
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            LetsEncrypt::Production.url(),
            None,
        )
        .await
        .map_err(|e| format!("failed to create ACME account: {e}"))?;

        // Save account credentials
        std::fs::create_dir_all(CERTS_DIR)
            .map_err(|e| format!("failed to create certs dir: {e}"))?;
        let creds_json =
            serde_json::to_string_pretty(&creds).map_err(|e| format!("serialize error: {e}"))?;
        std::fs::write(&account_path, creds_json)
            .map_err(|e| format!("failed to save account: {e}"))?;
        account
    };

    // Create order
    let identifier = Identifier::Dns(hostname.to_string());
    let mut order = account
        .new_order(&NewOrder {
            identifiers: &[identifier],
        })
        .await
        .map_err(|e| format!("failed to create order: {e}"))?;

    let state = order.state();
    if matches!(state.status, OrderStatus::Invalid) {
        return Err("ACME order is invalid".to_string());
    }

    // Get authorizations and set up HTTP-01 challenge
    let authorizations = order
        .authorizations()
        .await
        .map_err(|e| format!("failed to get authorizations: {e}"))?;

    let mut challenge_tokens = Vec::new();

    for auth in &authorizations {
        if matches!(auth.status, AuthorizationStatus::Valid) {
            continue;
        }

        let challenge = auth
            .challenges
            .iter()
            .find(|c| c.r#type == ChallengeType::Http01)
            .ok_or_else(|| "no HTTP-01 challenge found".to_string())?;

        let key_auth = order.key_authorization(challenge);
        let token = challenge.token.clone();

        info!(hostname, token = %token, "setting ACME challenge");
        challenge_store
            .set(token.clone(), key_auth.as_str().to_string())
            .await;
        challenge_tokens.push(token);

        // Tell ACME server we're ready
        order
            .set_challenge_ready(&challenge.url)
            .await
            .map_err(|e| format!("failed to set challenge ready: {e}"))?;
    }

    // Poll for order to become ready
    let mut tries = 0;
    let max_tries = 20;
    loop {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let state = order
            .refresh()
            .await
            .map_err(|e| format!("failed to refresh order: {e}"))?;

        match state.status {
            OrderStatus::Ready => break,
            OrderStatus::Valid => break,
            OrderStatus::Invalid => {
                // Cleanup challenge tokens
                for token in &challenge_tokens {
                    challenge_store.remove(token).await;
                }
                return Err("ACME order became invalid".to_string());
            }
            OrderStatus::Pending => {
                tries += 1;
                if tries >= max_tries {
                    for token in &challenge_tokens {
                        challenge_store.remove(token).await;
                    }
                    return Err("ACME order timed out (still pending)".to_string());
                }
            }
            OrderStatus::Processing => {
                tries += 1;
                if tries >= max_tries {
                    for token in &challenge_tokens {
                        challenge_store.remove(token).await;
                    }
                    return Err("ACME order timed out (processing)".to_string());
                }
            }
        }
    }

    // Cleanup challenge tokens
    for token in &challenge_tokens {
        challenge_store.remove(token).await;
    }

    // Generate private key and CSR
    let key_pair = KeyPair::generate().map_err(|e| format!("failed to generate key: {e}"))?;
    let mut params = CertificateParams::new(vec![hostname.to_string()])
        .map_err(|e| format!("failed to create cert params: {e}"))?;
    params.distinguished_name = DistinguishedName::new();

    let csr = params
        .serialize_request(&key_pair)
        .map_err(|e| format!("failed to create CSR: {e}"))?;
    let csr_der = csr.der();

    // Finalize order
    order
        .finalize(csr_der)
        .await
        .map_err(|e| format!("failed to finalize order: {e}"))?;

    // Wait for certificate
    let mut tries = 0;
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let state = order
            .refresh()
            .await
            .map_err(|e| format!("failed to refresh order: {e}"))?;
        match state.status {
            OrderStatus::Valid => break,
            OrderStatus::Invalid => return Err("order became invalid after finalize".to_string()),
            _ => {
                tries += 1;
                if tries >= 10 {
                    return Err("timed out waiting for certificate".to_string());
                }
            }
        }
    }

    let cert_chain_pem = order
        .certificate()
        .await
        .map_err(|e| format!("failed to download cert: {e}"))?
        .ok_or_else(|| "no certificate returned".to_string())?;

    let key_pem = key_pair.serialize_pem();

    // Save to disk
    std::fs::create_dir_all(&cert_dir)
        .map_err(|e| format!("failed to create cert dir: {e}"))?;
    std::fs::write(&cert_path, &cert_chain_pem)
        .map_err(|e| format!("failed to write cert: {e}"))?;
    std::fs::write(&key_path, &key_pem).map_err(|e| format!("failed to write key: {e}"))?;

    // Load into cert store
    cert_store
        .add_cert_from_pem(hostname, cert_chain_pem.as_bytes(), key_pem.as_bytes())
        .await?;

    info!(hostname, "ACME certificate issued and loaded");
    Ok(())
}

/// Background task: periodically check and renew ACME certs.
pub async fn renewal_loop(
    cert_store: Arc<DynamicCertStore>,
    challenge_store: ChallengeStore,
    acme_configs: Arc<RwLock<Vec<(String, String)>>>, // Vec<(hostname, email)>
) {
    // Check every 12 hours
    let mut interval = tokio::time::interval(Duration::from_secs(12 * 3600));
    interval.tick().await; // skip first immediate tick

    loop {
        interval.tick().await;

        let configs = acme_configs.read().await.clone();
        for (hostname, email) in &configs {
            if let Err(e) = issue_cert(email, hostname, &challenge_store, &cert_store).await {
                warn!(hostname = %hostname, error = %e, "ACME renewal failed");
            }
        }
    }
}
