//! JWT authentication for A2A protocol.
//!
//! Each deskd instance has an Ed25519 key pair. The public key is published
//! in the Agent Card JWKS. Outgoing requests are signed with JWT; incoming
//! requests are verified against the sender's published public key.

use anyhow::{Context, Result};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use ring::signature::KeyPair as _;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// JWT claims for A2A requests.
#[derive(Debug, Serialize, Deserialize)]
pub struct A2aClaims {
    /// Issuer — the sender's A2A URL.
    pub iss: String,
    /// Issued-at timestamp (Unix seconds).
    pub iat: u64,
    /// Expiry timestamp (Unix seconds).
    pub exp: u64,
}

/// A loaded Ed25519 key pair for signing JWTs.
pub struct KeyPair {
    /// DER-encoded PKCS#8 private key.
    private_key_der: Vec<u8>,
    /// Raw 32-byte public key.
    public_key_bytes: Vec<u8>,
}

impl KeyPair {
    /// Generate a new Ed25519 key pair.
    pub fn generate() -> Result<Self> {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng)
            .map_err(|e| anyhow::anyhow!("key generation failed: {}", e))?;

        let key_pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
            .map_err(|e| anyhow::anyhow!("key pair creation failed: {}", e))?;

        Ok(Self {
            private_key_der: pkcs8.as_ref().to_vec(),
            public_key_bytes: key_pair.public_key().as_ref().to_vec(),
        })
    }

    /// Load from PEM files on disk.
    pub fn load(private_key_path: &Path) -> Result<Self> {
        let pem_bytes = std::fs::read(private_key_path)
            .with_context(|| format!("reading private key: {}", private_key_path.display()))?;

        // Parse PEM to extract DER bytes.
        let pem_str = String::from_utf8_lossy(&pem_bytes);
        let der = pem_to_der(&pem_str).with_context(|| "invalid PEM format for private key")?;

        let key_pair = ring::signature::Ed25519KeyPair::from_pkcs8(&der)
            .map_err(|e| anyhow::anyhow!("invalid Ed25519 private key: {}", e))?;

        Ok(Self {
            private_key_der: der,
            public_key_bytes: key_pair.public_key().as_ref().to_vec(),
        })
    }

    /// Save key pair to PEM files.
    pub fn save(&self, private_key_path: &Path) -> Result<()> {
        if let Some(parent) = private_key_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let pem = der_to_pem(&self.private_key_der, "PRIVATE KEY");
        std::fs::write(private_key_path, pem.as_bytes())?;

        // Save public key alongside.
        let pub_path = private_key_path.with_extension("pub");
        let pub_pem = der_to_pem(&self.public_key_bytes, "PUBLIC KEY");
        std::fs::write(pub_path, pub_pem.as_bytes())?;

        Ok(())
    }

    /// Get the raw public key bytes (32 bytes for Ed25519).
    pub fn public_key_bytes(&self) -> &[u8] {
        &self.public_key_bytes
    }

    /// Get the public key as URL-safe base64 (for JWKS `x` field).
    pub fn public_key_base64url(&self) -> String {
        base64_url_encode(&self.public_key_bytes)
    }

    /// Sign a JWT with this key pair.
    pub fn sign_jwt(&self, issuer: &str, ttl_secs: u64) -> Result<String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let claims = A2aClaims {
            iss: issuer.to_string(),
            iat: now,
            exp: now + ttl_secs,
        };

        let header = Header::new(Algorithm::EdDSA);
        let key = EncodingKey::from_ed_der(&self.private_key_der);
        let token = jsonwebtoken::encode(&header, &claims, &key)?;
        Ok(token)
    }
}

/// Verify a JWT token against a raw Ed25519 public key.
pub fn verify_jwt(token: &str, public_key_bytes: &[u8]) -> Result<A2aClaims> {
    let key = DecodingKey::from_ed_der(public_key_bytes);
    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.validate_exp = true;
    validation.required_spec_claims.clear();
    validation.set_required_spec_claims(&["iss", "exp"]);

    let data = jsonwebtoken::decode::<A2aClaims>(token, &key, &validation)?;
    Ok(data.claims)
}

/// Verify a JWT using a base64url-encoded public key (from Agent Card JWKS).
pub fn verify_jwt_base64(token: &str, public_key_b64: &str) -> Result<A2aClaims> {
    let bytes = base64_url_decode(public_key_b64)?;
    verify_jwt(token, &bytes)
}

/// JWKS key entry for Agent Card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwkKey {
    pub kty: String,
    pub crv: String,
    /// Public key bytes, base64url-encoded.
    pub x: String,
}

/// JWKS key set for Agent Card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Jwks {
    pub keys: Vec<JwkKey>,
}

impl Jwks {
    /// Create JWKS from a public key.
    pub fn from_public_key(public_key_bytes: &[u8]) -> Self {
        Self {
            keys: vec![JwkKey {
                kty: "OKP".to_string(),
                crv: "Ed25519".to_string(),
                x: base64_url_encode(public_key_bytes),
            }],
        }
    }
}

// ─── PEM helpers ────────────────────────────────────────────────────────────

fn pem_to_der(pem: &str) -> Result<Vec<u8>> {
    let b64: String = pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<Vec<_>>()
        .join("");
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(&b64)
        .map_err(|e| anyhow::anyhow!("base64 decode failed: {}", e))
}

fn der_to_pem(der: &[u8], label: &str) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let mut pem = format!("-----BEGIN {}-----\n", label);
    for chunk in b64.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(chunk).unwrap());
        pem.push('\n');
    }
    pem.push_str(&format!("-----END {}-----\n", label));
    pem
}

fn base64_url_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn base64_url_decode(s: &str) -> Result<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|e| anyhow::anyhow!("base64url decode failed: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_and_sign_verify() {
        let kp = KeyPair::generate().unwrap();
        let token = kp.sign_jwt("https://test.example.com", 60).unwrap();

        let claims = verify_jwt(&token, kp.public_key_bytes()).unwrap();
        assert_eq!(claims.iss, "https://test.example.com");
        assert!(claims.exp > claims.iat);
        assert_eq!(claims.exp - claims.iat, 60);
    }

    #[test]
    fn test_verify_with_wrong_key_fails() {
        let kp1 = KeyPair::generate().unwrap();
        let kp2 = KeyPair::generate().unwrap();
        let token = kp1.sign_jwt("https://a.example.com", 60).unwrap();

        let result = verify_jwt(&token, kp2.public_key_bytes());
        assert!(result.is_err());
    }

    #[test]
    fn test_expired_token_rejected() {
        let kp = KeyPair::generate().unwrap();
        // Token that expired 100 seconds ago.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let claims = A2aClaims {
            iss: "https://test.example.com".into(),
            iat: now - 200,
            exp: now - 100,
        };
        let header = Header::new(Algorithm::EdDSA);
        let key = EncodingKey::from_ed_der(&kp.private_key_der);
        let token = jsonwebtoken::encode(&header, &claims, &key).unwrap();

        let result = verify_jwt(&token, kp.public_key_bytes());
        assert!(result.is_err());
    }

    #[test]
    fn test_base64url_roundtrip() {
        let kp = KeyPair::generate().unwrap();
        let b64 = kp.public_key_base64url();
        let token = kp.sign_jwt("https://test.example.com", 60).unwrap();

        let claims = verify_jwt_base64(&token, &b64).unwrap();
        assert_eq!(claims.iss, "https://test.example.com");
    }

    #[test]
    fn test_jwks_from_public_key() {
        let kp = KeyPair::generate().unwrap();
        let jwks = Jwks::from_public_key(kp.public_key_bytes());
        assert_eq!(jwks.keys.len(), 1);
        assert_eq!(jwks.keys[0].kty, "OKP");
        assert_eq!(jwks.keys[0].crv, "Ed25519");
        assert!(!jwks.keys[0].x.is_empty());
    }

    #[test]
    fn test_save_and_load_keypair() {
        let dir = std::env::temp_dir().join("deskd-test-jwt");
        let key_path = dir.join("test_key.pem");

        let kp = KeyPair::generate().unwrap();
        kp.save(&key_path).unwrap();

        let loaded = KeyPair::load(&key_path).unwrap();

        // Sign with original, verify with loaded.
        let token = kp.sign_jwt("https://test.example.com", 60).unwrap();
        let claims = verify_jwt(&token, loaded.public_key_bytes()).unwrap();
        assert_eq!(claims.iss, "https://test.example.com");

        // Sign with loaded, verify with original.
        let token2 = loaded.sign_jwt("https://test2.example.com", 30).unwrap();
        let claims2 = verify_jwt(&token2, kp.public_key_bytes()).unwrap();
        assert_eq!(claims2.iss, "https://test2.example.com");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_pem_roundtrip() {
        let original = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        let pem = der_to_pem(&original, "TEST DATA");
        let decoded = pem_to_der(&pem).unwrap();
        assert_eq!(original, decoded);
    }
}
