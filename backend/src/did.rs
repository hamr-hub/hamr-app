use anyhow::{Context, Result};
use ed25519_dalek::{SigningKey, VerifyingKey, Signer, Verifier, Signature};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::fs;

/// 设备身份，基于 Ed25519 密钥对
/// DID 格式：did:key:<multibase-encoded-public-key>
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceIdentity {
    /// 去中心化身份标识符（公钥派生）
    pub did: String,
    /// 私钥字节（序列化时跳过）
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    secret_key: Vec<u8>,
    /// 公钥字节
    public_key: Vec<u8>,
}

impl DeviceIdentity {
    /// 生成新的设备身份（Ed25519 密钥对）
    pub fn generate() -> Result<Self> {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();

        let public_key_bytes = verifying_key.to_bytes().to_vec();
        let secret_key_bytes = signing_key.to_bytes().to_vec();

        let did = Self::public_key_to_did(&verifying_key);

        Ok(Self {
            did,
            secret_key: secret_key_bytes,
            public_key: public_key_bytes,
        })
    }

    /// 从公钥生成 DID（did:key 格式）
    pub fn public_key_to_did(verifying_key: &VerifyingKey) -> String {
        let encoded = multibase::encode(
            multibase::Base::Base58Btc,
            verifying_key.as_bytes(),
        );
        format!("did:key:{}", encoded)
    }

    /// 获取签名密钥
    pub fn signing_key(&self) -> Result<SigningKey> {
        let bytes: [u8; 32] = self.secret_key
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid secret key length (expected 32 bytes)"))?;
        Ok(SigningKey::from_bytes(&bytes))
    }

    /// 获取验证密钥
    pub fn verifying_key(&self) -> Result<VerifyingKey> {
        let bytes: [u8; 32] = self.public_key
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid public key length (expected 32 bytes)"))?;
        VerifyingKey::from_bytes(&bytes)
            .map_err(|e| anyhow::anyhow!("Invalid public key: {}", e))
    }

    /// 对数据签名
    pub fn sign(&self, data: &[u8]) -> Result<Vec<u8>> {
        let signing_key = self.signing_key()?;
        let signature: Signature = signing_key.sign(data);
        Ok(signature.to_bytes().to_vec())
    }

    /// 验证签名
    pub fn verify(public_key_bytes: &[u8], data: &[u8], signature_bytes: &[u8]) -> Result<bool> {
        let key_arr: [u8; 32] = public_key_bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid public key length"))?;
        let sig_arr: [u8; 64] = signature_bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid signature length"))?;

        let verifying_key = VerifyingKey::from_bytes(&key_arr)
            .map_err(|e| anyhow::anyhow!("Invalid public key: {}", e))?;
        let signature = Signature::from_bytes(&sig_arr);

        Ok(verifying_key.verify(data, &signature).is_ok())
    }

    /// 从文件加载身份；若不存在则生成并保存
    pub async fn load_or_create(path: &Path) -> Result<Self> {
        if path.exists() {
            let content = fs::read(path)
                .await
                .context("Failed to read identity file")?;
            let identity: DeviceIdentity = serde_json::from_slice(&content)
                .context("Failed to parse identity file")?;
            tracing::info!("Loaded device identity from {:?}", path);
            Ok(identity)
        } else {
            let identity = Self::generate()?;
            identity.save(path).await?;
            tracing::info!("Generated new device identity: {}", identity.did);
            Ok(identity)
        }
    }

    /// 持久化保存身份到文件
    pub async fn save(&self, path: &Path) -> Result<()> {
        // 序列化时包含私钥
        let full = serde_json::json!({
            "did": self.did,
            "secret_key": self.secret_key,
            "public_key": self.public_key,
        });
        let content = serde_json::to_vec_pretty(&full)
            .context("Failed to serialize identity")?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .context("Failed to create identity directory")?;
        }

        fs::write(path, content)
            .await
            .context("Failed to write identity file")?;

        Ok(())
    }

    /// 公钥的 base64 编码（用于 API 响应）
    pub fn public_key_base64(&self) -> String {
        base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &self.public_key,
        )
    }
}

/// 经过 DID 签名的消息结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedMessage {
    pub device_id: String,
    pub timestamp: i64,
    pub payload: serde_json::Value,
    pub signature: String,
}

impl SignedMessage {
    pub fn new(
        device_id: String,
        payload: serde_json::Value,
        identity: &DeviceIdentity,
    ) -> Result<Self> {
        let timestamp = chrono::Utc::now().timestamp();
        let message = format!(
            "{}:{}:{}",
            device_id,
            timestamp,
            serde_json::to_string(&payload)?
        );
        let signature_bytes = identity.sign(message.as_bytes())?;
        let signature = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &signature_bytes,
        );

        Ok(Self {
            device_id,
            timestamp,
            payload,
            signature,
        })
    }

    pub fn verify(&self, public_key: &[u8]) -> Result<bool> {
        let message = format!(
            "{}:{}:{}",
            self.device_id,
            self.timestamp,
            serde_json::to_string(&self.payload)?
        );
        let signature_bytes = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            &self.signature,
        )
        .context("Invalid signature encoding")?;

        DeviceIdentity::verify(public_key, message.as_bytes(), &signature_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identity_generation() {
        let identity = DeviceIdentity::generate().unwrap();
        assert!(identity.did.starts_with("did:key:"));
        assert_eq!(identity.secret_key.len(), 32);
        assert_eq!(identity.public_key.len(), 32);
    }

    #[test]
    fn test_sign_and_verify() {
        let identity = DeviceIdentity::generate().unwrap();
        let data = b"test message";

        let signature = identity.sign(data).unwrap();
        let valid = DeviceIdentity::verify(&identity.public_key, data, &signature).unwrap();
        assert!(valid);

        let invalid =
            DeviceIdentity::verify(&identity.public_key, b"wrong message", &signature).unwrap();
        assert!(!invalid);
    }

    #[test]
    fn test_signed_message() {
        let identity = DeviceIdentity::generate().unwrap();
        let payload = serde_json::json!({ "action": "sync", "table": "people" });

        let message =
            SignedMessage::new(identity.did.clone(), payload.clone(), &identity).unwrap();
        assert!(message.verify(&identity.public_key).unwrap());
    }
}
