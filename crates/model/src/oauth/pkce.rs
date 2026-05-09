//! PKCE (Proof Key for Code Exchange) helpers.
//!
//! Mirrors `pi-mono/.../oauth/pkce.ts`. The verifier is a 32-byte random value
//! base64url-encoded (~43 chars), and the challenge is the base64url-encoded
//! SHA-256 of the verifier bytes.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// PKCE verifier/challenge pair.
pub struct PkcePair {
    /// URL-safe random string between 43 and 128 characters.
    pub verifier: String,
    /// `base64url(sha256(verifier))`, no padding.
    pub challenge: String,
}

/// Generate a fresh PKCE pair using OS randomness.
pub fn generate_pkce() -> PkcePair {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let verifier = URL_SAFE_NO_PAD.encode(bytes);

    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let digest = hasher.finalize();
    let challenge = URL_SAFE_NO_PAD.encode(digest);

    PkcePair {
        verifier,
        challenge,
    }
}
