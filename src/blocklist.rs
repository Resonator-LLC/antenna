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
/// **Production key** for the CMP-024 default subscribable blocklist (minted
/// 2026-06-23, CUT-25). The matching secret seed is generated offline and lives
/// **only** in the developer's 1Password — never in the repo — at
/// `op://Personal/Resonator blocklist signing key (Ed25519)/credential`.
/// Rotating means re-signing `blocklist.json` with the new secret and updating
/// these bytes; the custody + rotation procedure is
/// `compliance/plan/cmp024-blocklist-key.md`. The positive test below pins
/// these bytes against the exact signed artifact served at
/// `https://resonator.network/blocklist.json`.
pub const BLOCKLIST_PUBKEY: [u8; 32] = [
    0xe7, 0x13, 0x5a, 0xd6, 0x39, 0x8a, 0xdb, 0x9c, 0x0a, 0xcd, 0x80, 0xb1, 0xf8, 0xf9, 0x61, 0xd0,
    0x64, 0xcb, 0xe1, 0xbd, 0x94, 0x33, 0x3d, 0x32, 0xc4, 0xd3, 0xcb, 0x0c, 0xaa, 0xc4, 0x22, 0x06,
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

    /// The exact signed artifact served at
    /// `https://resonator.network/blocklist.json` — the seed list, which blocks
    /// nobody. **Non-secret**: this is the public payload + detached signature
    /// (the signing secret lives only in 1Password — see the module doc). If the
    /// hosted seed list is re-signed, copy the new `payload`/`sig` out of
    /// `blocklist.json` into these constants and re-run the test.
    const SEED_PAYLOAD_B64: &str = "IyBSZXNvbmF0b3IgZGVmYXVsdCBibG9ja2xpc3Qg4oCUIHYxCiMKIyBPbmUgaWRlbnRpdHkgVVJJIHBlciBsaW5lLiBCbGFuayBsaW5lcyBhbmQgbGluZXMgYmVnaW5uaW5nIHdpdGggJyMnIGFyZQojIGlnbm9yZWQuIFN1YnNjcmlwdGlvbiBpcyBvcHQtaW4gYW5kIE9GRiBieSBkZWZhdWx0OyB2ZXJpZmllZCBlbnRyaWVzIGFyZQojIGFwcGxpZWQgdGhyb3VnaCB0aGUgc2FtZSByZW5kZXIgZ2F0ZSArIGNhcnJpZXIgYmFuIGFzIGEgbWFudWFsIGJsb2NrLCBhbmQgYW55CiMgZW50cnkgY2FuIGJlIG92ZXJyaWRkZW4gKHVuYmxvY2tlZCkgbG9jYWxseS4KIwojIFRoaXMgaW5pdGlhbCBsaXN0IGlzIGludGVudGlvbmFsbHkgZW1wdHkg4oCUIGl0IGJsb2NrcyBubyBpZGVudGl0aWVzLiBJdHMgc29sZQojIHB1cnBvc2UgaXMgdG8gZXN0YWJsaXNoIHRoZSBzaWduZWQtZGlzdHJpYnV0aW9uIHBpcGVsaW5lIGVuZCB0byBlbmQuCg==";
    const SEED_SIG_B64: &str =
        "L3ScEW5TIWmi2/FZEh+DlmM2cdf2urVkaivY34cKQa5bGFfki+MFNCqZwhaDboH9OUlGMhfvWrNJcHOKDjU5AQ==";

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    /// Sign `payload` with `key`, returning `(payload_b64, sig_b64)`.
    fn sign(key: &SigningKey, payload: &str) -> (String, String) {
        let sig = key.sign(payload.as_bytes());
        (b64(payload.as_bytes()), b64(&sig.to_bytes()))
    }

    /// The pinned production key verifies the exact artifact we publish, and the
    /// seed list parses to an empty entry set (it blocks no identities). A
    /// failure means either [`BLOCKLIST_PUBKEY`] is stale or `blocklist.json`
    /// was re-signed without updating the sample constants above.
    #[test]
    fn pinned_key_verifies_published_seed_list() {
        let entries = verify_signed_blocklist(SEED_PAYLOAD_B64, SEED_SIG_B64, &BLOCKLIST_PUBKEY)
            .expect("published seed list must verify against the pinned production key");
        assert!(entries.is_empty(), "seed list blocks nobody; got {entries:?}");
    }

    /// Tampering the published payload while reusing its signature fails against
    /// the pinned key — verified content cannot be substituted.
    #[test]
    fn tampered_published_payload_is_rejected() {
        let tampered = b64(b"identity-evil\n");
        let err = verify_signed_blocklist(&tampered, SEED_SIG_B64, &BLOCKLIST_PUBKEY).unwrap_err();
        assert!(err.to_string().contains("does not verify"), "got: {err}");
    }

    /// A list signed by any other key is rejected — only the pinned developer
    /// key is trusted.
    #[test]
    fn wrong_key_is_rejected() {
        let attacker = SigningKey::from_bytes(&[0x07u8; 32]);
        let (p, s) = sign(&attacker, "identity-aaa\n");
        let err = verify_signed_blocklist(&p, &s, &BLOCKLIST_PUBKEY).unwrap_err();
        assert!(err.to_string().contains("does not verify"), "got: {err}");
    }

    /// Parsing coverage: a validly-signed multi-line list returns its entries
    /// with blank lines and `#` comments dropped. Signed by an ephemeral key and
    /// verified against that same key — exercises `parse_entries` without the
    /// production secret.
    #[test]
    fn valid_list_parses_entries() {
        let key = SigningKey::from_bytes(&[0x42u8; 32]);
        let payload = "# header\nidentity-aaa\nidentity-bbb\n\nidentity-ccc\n";
        let (p, s) = sign(&key, payload);
        let pk = key.verifying_key().to_bytes();
        let entries = verify_signed_blocklist(&p, &s, &pk).expect("verify");
        assert_eq!(entries, vec!["identity-aaa", "identity-bbb", "identity-ccc"]);
    }

    #[test]
    fn garbage_base64_is_rejected() {
        let err =
            verify_signed_blocklist("!!not-base64!!", "also-bad", &BLOCKLIST_PUBKEY).unwrap_err();
        assert!(err.to_string().contains("base64"), "got: {err}");
    }
}
