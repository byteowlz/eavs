use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

pub fn generate_pkce_pair() -> (String, String) {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    let challenge = code_challenge(&verifier).unwrap_or_default();
    (verifier, challenge)
}

pub fn code_challenge(verifier: &str) -> Result<String, String> {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let digest = hasher.finalize();
    Ok(URL_SAFE_NO_PAD.encode(digest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pkce_challenge_known_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let expected = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        let challenge = code_challenge(verifier).unwrap();
        assert_eq!(challenge, expected);
    }

    #[test]
    fn test_pkce_pair_lengths() {
        let (verifier, challenge) = generate_pkce_pair();
        assert!(verifier.len() >= 43);
        assert!(challenge.len() >= 43);
    }
}
