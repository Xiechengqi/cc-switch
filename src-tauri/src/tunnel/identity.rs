use super::config::TunnelConfig;
use super::error::TunnelError;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::io::Write;

#[derive(Debug, Clone)]
pub struct TunnelIdentity {
    pub installation_id: String,
    pub signing_key: SigningKey,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredIdentity {
    installation_id: String,
    private_key_base64: String,
    public_key_base64: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RegisterInstallationRequest<'a> {
    public_key: &'a str,
    platform: &'a str,
    app_version: &'a str,
    instance_nonce: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisterInstallationResponse {
    installation_id: String,
}

pub async fn ensure_identity(
    client: &reqwest::Client,
    config: &TunnelConfig,
) -> Result<TunnelIdentity, TunnelError> {
    match load_identity() {
        Ok(Some(identity)) => return Ok(identity),
        Ok(None) => {}
        Err(err) => {
            log::warn!("[TunnelIdentity] invalid stored identity, resetting: {err}");
            reset_identity()?;
        }
    }

    let signing_key = SigningKey::generate(&mut OsRng);
    let public_key_base64 =
        base64::engine::general_purpose::STANDARD.encode(signing_key.verifying_key().to_bytes());
    let private_key_base64 =
        base64::engine::general_purpose::STANDARD.encode(signing_key.to_bytes());

    let payload = RegisterInstallationRequest {
        public_key: &public_key_base64,
        platform: std::env::consts::OS,
        app_version: env!("CARGO_PKG_VERSION"),
        instance_nonce: uuid::Uuid::new_v4().to_string(),
    };

    let url = format!("{}/v1/installations/register", config.get_server_addr());
    let response = client
        .post(url)
        .json(&payload)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| TunnelError::Api(format!("register installation failed: {e}")))?;

    if !response.status().is_success() {
        return Err(TunnelError::Api(format!(
            "register installation failed: HTTP {}",
            response.status()
        )));
    }

    let body: RegisterInstallationResponse = response
        .json()
        .await
        .map_err(|e| TunnelError::Api(format!("parse installation response failed: {e}")))?;

    let stored = StoredIdentity {
        installation_id: body.installation_id.clone(),
        private_key_base64,
        public_key_base64: public_key_base64.clone(),
    };
    save_identity(&stored)?;

    Ok(TunnelIdentity {
        installation_id: body.installation_id,
        signing_key,
    })
}

pub fn reset_identity() -> Result<(), TunnelError> {
    let path = identity_path()?;
    if !path.exists() {
        return Ok(());
    }
    std::fs::remove_file(&path)
        .map_err(|e| TunnelError::Other(format!("remove tunnel identity failed: {e}")))?;
    Ok(())
}

pub fn sign_lease_payload(
    identity: &TunnelIdentity,
    installation_id: &str,
    requested_subdomain: &str,
    tunnel_type: &str,
    timestamp_ms: i64,
    nonce: &str,
) -> String {
    let payload =
        format!("{installation_id}\n{requested_subdomain}\n{tunnel_type}\n{timestamp_ms}\n{nonce}");
    let signature = identity.signing_key.sign(payload.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(signature.to_bytes())
}

pub fn sign_action_payload<T: Serialize>(
    identity: &TunnelIdentity,
    installation_id: &str,
    action: &str,
    payload: &T,
    timestamp_ms: i64,
    nonce: &str,
) -> Result<String, TunnelError> {
    let payload_json = serde_json::to_string(payload)
        .map_err(|e| TunnelError::Other(format!("serialize signed payload failed: {e}")))?;
    let body = format!("{installation_id}\n{action}\n{payload_json}\n{timestamp_ms}\n{nonce}");
    let signature = identity.signing_key.sign(body.as_bytes());
    Ok(base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()))
}

pub fn should_reset_identity_for_api_error(message: &str) -> bool {
    message.contains("installation not found") || message.contains("signature verification failed")
}

fn load_identity() -> Result<Option<TunnelIdentity>, TunnelError> {
    let path = identity_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| TunnelError::Other(format!("read tunnel identity failed: {e}")))?;
    let stored: StoredIdentity = serde_json::from_str(&raw)
        .map_err(|e| TunnelError::Other(format!("parse tunnel identity failed: {e}")))?;
    let private_bytes = base64::engine::general_purpose::STANDARD
        .decode(&stored.private_key_base64)
        .map_err(|e| TunnelError::Other(format!("decode tunnel private key failed: {e}")))?;
    let private_array: [u8; 32] = private_bytes
        .try_into()
        .map_err(|_| TunnelError::Other("invalid tunnel private key length".into()))?;
    let signing_key = SigningKey::from_bytes(&private_array);
    validate_stored_identity(&stored, &signing_key)?;
    Ok(Some(TunnelIdentity {
        installation_id: stored.installation_id,
        signing_key,
    }))
}

fn validate_stored_identity(
    stored: &StoredIdentity,
    signing_key: &SigningKey,
) -> Result<(), TunnelError> {
    let derived_public_key_base64 =
        base64::engine::general_purpose::STANDARD.encode(signing_key.verifying_key().to_bytes());
    if stored.public_key_base64 != derived_public_key_base64 {
        return Err(TunnelError::Other(
            "stored tunnel identity is inconsistent: public key does not match private key".into(),
        ));
    }

    let public_bytes = base64::engine::general_purpose::STANDARD
        .decode(&stored.public_key_base64)
        .map_err(|e| TunnelError::Other(format!("decode tunnel public key failed: {e}")))?;
    let public_array: [u8; 32] = public_bytes
        .try_into()
        .map_err(|_| TunnelError::Other("invalid tunnel public key length".into()))?;
    VerifyingKey::from_bytes(&public_array)
        .map_err(|e| TunnelError::Other(format!("parse tunnel public key failed: {e}")))?;

    if stored.installation_id.trim().is_empty() {
        return Err(TunnelError::Other(
            "stored tunnel identity is inconsistent: installation id is empty".into(),
        ));
    }

    Ok(())
}

fn save_identity(stored: &StoredIdentity) -> Result<(), TunnelError> {
    let path = identity_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| TunnelError::Other(format!("create tunnel identity dir failed: {e}")))?;
    }
    let raw = serde_json::to_vec_pretty(stored)
        .map_err(|e| TunnelError::Other(format!("serialize tunnel identity failed: {e}")))?;
    atomic_write_identity(&path, &raw)?;
    Ok(())
}

fn identity_path() -> Result<std::path::PathBuf, TunnelError> {
    Ok(crate::config::get_home_dir()
        .join(".cc-switch")
        .join("tunnel-identity.json"))
}

fn atomic_write_identity(path: &std::path::Path, data: &[u8]) -> Result<(), TunnelError> {
    let tmp_path = path.with_extension("json.tmp");
    let mut file = create_identity_file(&tmp_path)?;
    file.write_all(data)
        .and_then(|_| file.flush())
        .map_err(|e| TunnelError::Other(format!("write tunnel identity failed: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| TunnelError::Other(format!("chmod tunnel identity failed: {e}")))?;
    }
    std::fs::rename(&tmp_path, path)
        .map_err(|e| TunnelError::Other(format!("replace tunnel identity failed: {e}")))?;
    Ok(())
}

fn create_identity_file(path: &std::path::Path) -> Result<std::fs::File, TunnelError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        return std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| TunnelError::Other(format!("open tunnel identity file failed: {e}")));
    }

    #[cfg(not(unix))]
    {
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .map_err(|e| TunnelError::Other(format!("open tunnel identity file failed: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_stored_identity(signing_key: &SigningKey) -> StoredIdentity {
        StoredIdentity {
            installation_id: "installation-1".to_string(),
            private_key_base64: base64::engine::general_purpose::STANDARD
                .encode(signing_key.to_bytes()),
            public_key_base64: base64::engine::general_purpose::STANDARD
                .encode(signing_key.verifying_key().to_bytes()),
        }
    }

    #[test]
    fn validates_consistent_stored_identity() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let stored = sample_stored_identity(&signing_key);
        assert!(validate_stored_identity(&stored, &signing_key).is_ok());
    }

    #[test]
    fn rejects_mismatched_public_key() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let other_key = SigningKey::generate(&mut OsRng);
        let mut stored = sample_stored_identity(&signing_key);
        stored.public_key_base64 =
            base64::engine::general_purpose::STANDARD.encode(other_key.verifying_key().to_bytes());

        let err = validate_stored_identity(&stored, &signing_key).unwrap_err();
        assert!(err
            .to_string()
            .contains("public key does not match private key"));
    }

    #[test]
    fn resets_identity_for_signature_or_installation_errors() {
        assert!(should_reset_identity_for_api_error(
            "installation not found"
        ));
        assert!(should_reset_identity_for_api_error(
            "claim subdomain failed: signature verification failed"
        ));
        assert!(!should_reset_identity_for_api_error(
            "share subdomain is not claimed"
        ));
    }
}
