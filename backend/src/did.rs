use anyhow::{Result, Context};
use ed25519_dalek::{Keypair, PublicKey, SecretKey, Signer, Signature};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::fs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceIdentity {
    pub did: String,
    #[serde(skip_serializing)]
    secret_key: Vec<u8>,
    public_key: Vec<u8>,
}

impl DeviceIdentity {
    pub fn generate() -> Result<Self> {
        let mut csprng = OsRng {};
        let keypair = Keypair::generate(&mut csprng);
        
        let public_key_bytes = keypair.public.as_bytes().to_vec();
        let secret_key_bytes = keypair.secret.as_bytes().to_vec();
        
        let did = Self::public_key_to_did(&keypair.public);
        
        Ok(Self {
            did,
            secret_key: secret_key_bytes,
            public_key: public_key_bytes,
        })
    }
    
    pub fn public_key_to_did(public_key: &PublicKey) -> String {
        let encoded = multibase::encode(multibase::Base::Base58Btc, public_key.as_bytes());
        format!("did:key:{}", encoded)
    }
    
    pub fn keypair(&self) -> Result<Keypair> {
        let secret = SecretKey::from_bytes(&self.secret_key)
            .map_err(|e| anyhow::anyhow!("Invalid secret key: {}", e))?;
        let public = PublicKey::from(&secret);
        Ok(Keypair { secret, public })
    }
    
    pub fn sign(&self, data: &[u8]) -> Result<Vec<u8>> {
        let keypair = self.keypair()?;
        let signature = keypair.sign(data);
        Ok(signature.to_bytes().to_vec())
    }
    
    pub fn verify(public_key: &[u8], data: &[u8], signature: &[u8]) -> Result<bool> {
        let public = PublicKey::from_bytes(public_key)
            .map_err(|e| anyhow::anyhow!("Invalid public key: {}", e))?;
        let sig = Signature::from_bytes(signature)
            .map_err(|e| anyhow::anyhow!("Invalid signature: {}", e))?;
        
        public.verify_strict(data, &sig)
            .map(|_| true)
            .or(Ok(false))
    }
    
    pub async fn load_or_create(path: &Path) -> Result<Self> {
        if path.exists() {
            let content = fs::read(path).await
                .context("Failed to read identity file")?;
            let identity: DeviceIdentity = serde_json::from_slice(&content)
                .context("Failed to parse identity file")?;
            Ok(identity)
        } else {
            let identity = Self::generate()?;
            identity.save(path).await?;
            Ok(identity)
        }
    }
    
    pub async fn save(&self, path: &Path) -> Result<()> {
        let content = serde_json::to_vec_pretty(self)
            .context("Failed to serialize identity")?;
        
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await
                .context("Failed to create directory")?;
        }
        
        fs::write(path, content).await
            .context("Failed to write identity file")?;
        
        Ok(())
    }
    
    pub fn public_key_base64(&self) -> String {
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &self.public_key)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedMessage {
    pub device_id: String,
    pub timestamp: i64,
    pub payload: serde_json::Value,
    pub signature: String,
}

impl SignedMessage {
    pub fn new(device_id: String, payload: serde_json::Value, identity: &DeviceIdentity) -> Result<Self> {
        let timestamp = chrono::Utc::now().timestamp();
        let message = format!("{}:{}:{}", device_id, timestamp, serde_json::to_string(&payload)?);
        let signature_bytes = identity.sign(message.as_bytes())?;
        let signature = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &signature_bytes);
        
        Ok(Self {
            device_id,
            timestamp,
            payload,
            signature,
        })
    }
    
    pub fn verify(&self, public_key: &[u8]) -> Result<bool> {
        let message = format!("{}:{}:{}", self.device_id, self.timestamp, serde_json::to_string(&self.payload)?);
        let signature_bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &self.signature)
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
        assert!(!identity.secret_key.is_empty());
        assert!(!identity.public_key.is_empty());
    }
    
    #[test]
    fn test_sign_and_verify() {
        let identity = DeviceIdentity::generate().unwrap();
        let data = b"test message";
        
        let signature = identity.sign(data).unwrap();
        let valid = DeviceIdentity::verify(&identity.public_key, data, &signature).unwrap();
        assert!(valid);
        
        let invalid = DeviceIdentity::verify(&identity.public_key, b"wrong message", &signature).unwrap();
        assert!(!invalid);
    }
    
    #[test]
    fn test_signed_message() {
        let identity = DeviceIdentity::generate().unwrap();
        let payload = serde_json::json!({ "action": "sync", "table": "people" });
        
        let message = SignedMessage::new(identity.did.clone(), payload.clone(), &identity).unwrap();
        assert!(message.verify(&identity.public_key).unwrap());
    }
}
