// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

//! CMP-024 — signed subscribable blocklist verification.
//!
//! On a serverless network there is no central ban. The credible
//! "ongoing, network-wide moderation" story (Matrix ban-list / Bluesky
//! labeler precedent) is a developer-maintained, client-subscribed list of
//! known-bad identities. To make subscribing safe, the list is **signed**: the
//! developer signs the exact payload bytes with an Ed25519 key, the client
//! pins the matching public key, and **antenna verifies the signature before a
//! single entry is applied**. Verified entries are then applied by the radio
//! pipeline through the same render gate + carrier ban as a manual block, so
//! there is one enforcement path.
//!
//! Verification lives here (Rust) rather than in the QuickJS pipeline because
//! the script VM has no crypto primitives. We keep it self-contained with the
//! pure-Rust `ed25519-dalek` rather than reaching across the carrier FFI.

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use ed25519_dalek::{Signature, VerifyingKey};

/// Pinned developer signing key (Ed25519 public key, 32 bytes).
///
/// **PLACEHOLDER — rotate before store submission.** These bytes were derived
/// for development from a fixed seed (see the test module); the matching secret
/// is dev-only and must be replaced with the real developer key whose secret
/// lives offline, never in the repo. Mirrors the placeholder convention of
/// `SUPPORT_EMAIL` and the privacy-policy URL (Track E / CMP-009).
pub const DEV_BLOCKLIST_PUBKEY: [u8; 32] = [
    0x21, 0x52, 0xf8, 0xd1, 0x9b, 0x79, 0x1d, 0x24, 0x45, 0x32, 0x42, 0xe1, 0x5f, 0x2e, 0xab, 0x6c,
    0xb7, 0xcf, 0xfa, 0x7b, 0x6a, 0x5e, 0xd3, 0x00, 0x97, 0x96, 0x0e, 0x06, 0x98, 0x81, 0xdb, 0x12,
];

/// Verify the detached Ed25519 signature `sig_b64` over the exact bytes of the
/// base64-encoded `payload_b64`, using `pubkey`. On success return the list of
/// blocked identity URIs: the decoded payload is a newline-separated list;
/// blank lines and `#` comment lines are ignored.
///
/// Returns `Err` (and the caller applies nothing) on any failure: malformed
/// base64, a key/signature of the wrong length, a non-UTF-8 payload, or a
/// signature that does not verify. `verify_strict` is used so malleable /
/// small-order signatures are rejected.
pub fn verify_signed_blocklist(
    payload_b64: &str,
    sig_b64: &str,
    pubkey: &[u8; 32],
) -> Result<Vec<String>> {
    let engine = base64::engine::general_purpose::STANDARD;
    let payload = engine
        .decode(payload_b64.trim().as_bytes())
        .context("blocklist payload is not valid base64")?;
    let sig_bytes = engine
        .decode(sig_b64.trim().as_bytes())
        .context("blocklist signature is not valid base64")?;
    let sig_arr: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("blocklist signature must be 64 bytes"))?;
    let signature = Signature::from_bytes(&sig_arr);
    let vk = VerifyingKey::from_bytes(pubkey).context("pinned blocklist key is invalid")?;
    vk.verify_strict(&payload, &signature)
        .map_err(|_| anyhow!("blocklist signature does not verify"))?;
    let text = String::from_utf8(payload).context("blocklist payload is not UTF-8")?;
    Ok(parse_entries(&text))
}

/// Split a verified payload into identity URIs, dropping blank and `#` lines.
fn parse_entries(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    /// Dev-only signing seed. The matching public key is pinned as
    /// [`DEV_BLOCKLIST_PUBKEY`]; the real key's secret is never committed.
    const DEV_SEED: [u8; 32] = [
        0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42,
        0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42,
        0x42, 0x42,
    ];

    fn dev_key() -> SigningKey {
        SigningKey::from_bytes(&DEV_SEED)
    }

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    /// Sign `payload` with `key`, returning `(payload_b64, sig_b64)`.
    fn sign(key: &SigningKey, payload: &str) -> (String, String) {
        let sig = key.sign(payload.as_bytes());
        (b64(payload.as_bytes()), b64(&sig.to_bytes()))
    }

    /// The pinned constant must equal the dev seed's public key. If this fails,
    /// re-bake `DEV_BLOCKLIST_PUBKEY` from the printed bytes.
    #[test]
    fn pinned_pubkey_matches_dev_seed() {
        let pk = dev_key().verifying_key().to_bytes();
        assert_eq!(
            pk, DEV_BLOCKLIST_PUBKEY,
            "DEV_BLOCKLIST_PUBKEY is stale; dev-seed pubkey = {pk:?}"
        );
    }

    #[test]
    fn valid_list_returns_entries() {
        let key = dev_key();
        let payload = "# resonator default blocklist\nidentity-aaa\nidentity-bbb\n\nidentity-ccc\n";
        let (p, s) = sign(&key, payload);
        let entries = verify_signed_blocklist(&p, &s, &DEV_BLOCKLIST_PUBKEY).expect("verify");
        assert_eq!(
            entries,
            vec!["identity-aaa", "identity-bbb", "identity-ccc"]
        );
    }

    #[test]
    fn tampered_payload_is_rejected() {
        let key = dev_key();
        let (_p, s) = sign(&key, "identity-aaa\nidentity-bbb\n");
        // Re-encode a different payload under the original signature.
        let tampered = b64(b"identity-aaa\nidentity-evil\n");
        let err = verify_signed_blocklist(&tampered, &s, &DEV_BLOCKLIST_PUBKEY).unwrap_err();
        assert!(err.to_string().contains("does not verify"), "got: {err}");
    }

    #[test]
    fn wrong_key_is_rejected() {
        let attacker = SigningKey::from_bytes(&[0x07u8; 32]);
        let (p, s) = sign(&attacker, "identity-aaa\n");
        // Signed by the attacker, verified against the pinned dev key → reject.
        let err = verify_signed_blocklist(&p, &s, &DEV_BLOCKLIST_PUBKEY).unwrap_err();
        assert!(err.to_string().contains("does not verify"), "got: {err}");
    }

    #[test]
    fn garbage_base64_is_rejected() {
        let err = verify_signed_blocklist("!!not-base64!!", "also-bad", &DEV_BLOCKLIST_PUBKEY)
            .unwrap_err();
        assert!(err.to_string().contains("base64"), "got: {err}");
    }
}
