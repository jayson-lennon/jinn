//! The Web Push protocol: an `aes128gcm` payload (RFC 8291) under a VAPID
//! signature (RFC 8292).
//!
//! Encryption keeps the push service from reading the notification — it sees
//! only the endpoint, the time, and the server's address. The signature
//! proves to the push service that the request came from the holder of the
//! key pair the subscription was made under. Every vendor's service accepts
//! both as they are specified, with nothing registered anywhere.
//!
//! The sender's ephemeral key and salt are parameters rather than drawn
//! inside, so the RFC's own test vector — which fixes both — can be
//! reproduced byte for byte.

use aes_gcm::{
    Aes128Gcm, KeyInit,
    aead::{Aead as _, Payload as AeadPayload},
};
use base64::Engine as _;
use hkdf::Hkdf;
use p256::ecdsa::{Signature, SigningKey, signature::Signer as _};
use p256::elliptic_curve::sec1::ToEncodedPoint as _;
use serde::Serialize;
use sha2::Sha256;

/// The record size the header declares: the RFC's maximum for a push body,
/// which every record this server sends is far below.
const RECORD_SIZE: u32 = 4096;

/// The HKDF info string for the content encryption key, as RFC 8188 spells it.
const CEK_INFO: &[u8] = b"Content-Encoding: aes128gcm\0";

/// The HKDF info string for the nonce, as RFC 8188 spells it.
const NONCE_INFO: &[u8] = b"Content-Encoding: nonce\0";

/// The length of the content encryption key: AES-128.
const CEK_LEN: usize = 16;

/// The length of the AES-GCM nonce.
const NONCE_LEN: usize = 12;

/// What a push is about: the session it names, and the title to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Payload {
    /// The session the notification names — what a tap activates.
    pub session_id: String,
    /// The session's title, as the banner shows it.
    pub title: String,
    /// The banner's body: a fixed line naming what happened.
    pub body: String,
}

/// A subscription's decryption material, as the browser issued it.
#[derive(Debug, Clone)]
pub struct Receiver {
    /// The subscription's P-256 public point, uncompressed and unpadded
    /// base64url, as the browser issued it.
    pub p256dh: String,
    /// The authentication secret, unpadded base64url, as the browser issued it.
    pub auth: String,
}

/// The encrypted record, as the request body carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedRecord {
    /// The header and ciphertext, concatenated: the `aes128gcm` body.
    pub body: Vec<u8>,
}

/// Why encryption failed.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub enum EncryptError {
    /// The subscription's keys are not valid base64url P-256 material.
    BadKeys,
}

/// The base64url engine every key and segment is coded with: unpadded.
fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
}

/// Encrypts `payload` for `receiver`, sealing it under the ephemeral key
/// `sender` and the `salt`.
///
/// # Errors
///
/// Returns [`EncryptError::BadKeys`] when the subscription's keys are not
/// valid base64url P-256 material.
pub fn encrypt(
    payload: &Payload,
    receiver: &Receiver,
    sender: &SigningKey,
    salt: &[u8; 16],
) -> Result<EncryptedRecord, error_stack::Report<EncryptError>> {
    encrypt_bytes(
        &serde_json::to_vec(payload).unwrap_or_default(),
        receiver,
        sender,
        salt,
    )
}

/// Seals an already-serialized payload. The RFC's test vector fixes its
/// plaintext, so the seam between "serialize" and "seal" is a function.
///
/// # Errors
///
/// Returns [`EncryptError::BadKeys`] when the subscription's keys are not
/// valid base64url P-256 material.
pub fn encrypt_bytes(
    plaintext: &[u8],
    receiver: &Receiver,
    sender: &SigningKey,
    salt: &[u8; 16],
) -> Result<EncryptedRecord, error_stack::Report<EncryptError>> {
    use error_stack::ResultExt as _;
    let ua_public = p256::PublicKey::from_sec1_bytes(
        &b64()
            .decode(&receiver.p256dh)
            .change_context(EncryptError::BadKeys)?,
    )
    .change_context(EncryptError::BadKeys)?;
    let as_public: p256::PublicKey = (*sender.verifying_key()).into();
    let (cek, nonce) = derive_cek_and_nonce(&ua_public, &as_public, sender, &receiver.auth, salt)?;
    Ok(EncryptedRecord {
        body: seal(&cek, &nonce, plaintext, &as_public, salt)?,
    })
}

/// The content encryption key and nonce, per RFC 8291 section 3.3 and
/// RFC 8188.
///
/// # Errors
///
/// Returns [`EncryptError::BadKeys`] when the subscription's `auth` secret
/// is not valid base64url, the same answer a bad `p256dh` key gets: either
/// one makes the browser fail to decrypt, and a push it cannot read is
/// silently lost. The HKDF calls themselves cannot fail on fixed-size
/// output, and their errors are carried rather than panicked on.
fn derive_cek_and_nonce(
    ua_public: &p256::PublicKey,
    as_public: &p256::PublicKey,
    sender: &SigningKey,
    auth: &str,
    salt: &[u8; 16],
) -> Result<
    (aes_gcm::Key<Aes128Gcm>, aes_gcm::aead::Nonce<Aes128Gcm>),
    error_stack::Report<EncryptError>,
> {
    use error_stack::ResultExt as _;
    // The HKDF crate's errors are `Display`-only, so `change_context` cannot
    // take them; this is the one conversion every stage below goes through.
    fn bad_keys(error: impl core::fmt::Display) -> error_stack::Report<EncryptError> {
        error_stack::Report::new(EncryptError::BadKeys).attach(error.to_string())
    }
    // ecdh_secret = ECDH(as_private, ua_public): the x coordinate of the
    // shared point, as 32 big-endian bytes.
    let shared = p256::elliptic_curve::ecdh::diffie_hellman(
        sender.as_nonzero_scalar(),
        ua_public.as_affine(),
    );
    let ecdh_secret = shared.raw_secret_bytes();
    // PRK_key = HMAC-SHA-256(auth_secret, ecdh_secret): `extract` is that
    // step, with the auth secret as the salt.
    let auth_secret = b64()
        .decode(auth)
        .change_context(EncryptError::BadKeys)
        .attach("the subscription's auth secret is not base64url")?;
    let (prk_key, _) = Hkdf::<Sha256>::extract(Some(&auth_secret), ecdh_secret.as_slice());
    // IKM = HKDF-Expand(PRK_key, key_info, 32)
    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(ua_public.to_encoded_point(false).as_bytes());
    key_info.extend_from_slice(as_public.to_encoded_point(false).as_bytes());
    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::from_prk(&prk_key)
        .map_err(bad_keys)?
        .expand(&key_info, &mut ikm)
        .map_err(bad_keys)?;
    // PRK = HMAC-SHA-256(salt, IKM)
    let (prk, _) = Hkdf::<Sha256>::extract(Some(salt), &ikm);
    let hk = Hkdf::<Sha256>::from_prk(&prk).map_err(bad_keys)?;
    let mut cek = [0u8; CEK_LEN];
    hk.expand(CEK_INFO, &mut cek).map_err(bad_keys)?;
    let mut nonce = [0u8; NONCE_LEN];
    hk.expand(NONCE_INFO, &mut nonce).map_err(bad_keys)?;
    Ok((
        *aes_gcm::Key::<Aes128Gcm>::from_slice(&cek),
        *aes_gcm::Nonce::from_slice(&nonce),
    ))
}

/// The `aes128gcm` record: header, then the AES-GCM seal of the plaintext
/// with the padding delimiter appended.
/// # Errors
///
/// Returns [`EncryptError::BadKeys`] if the record cannot be sealed, which
/// a well-formed key and nonce never allow; the error is carried rather
/// than panicked on so a bad subscription costs one push, not the process.
fn seal(
    cek: &aes_gcm::Key<Aes128Gcm>,
    nonce: &aes_gcm::aead::Nonce<Aes128Gcm>,
    plaintext: &[u8],
    as_public: &p256::PublicKey,
    salt: &[u8; 16],
) -> Result<Vec<u8>, error_stack::Report<EncryptError>> {
    let sender_point = as_public.to_encoded_point(false);
    let keyid = sender_point.as_bytes();
    let mut header = Vec::with_capacity(21 + keyid.len());
    header.extend_from_slice(salt);
    header.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    // 65, an uncompressed P-256 point: one byte, always.
    let keyid_len = u8::try_from(keyid.len()).map_err(|error| {
        error_stack::Report::new(EncryptError::BadKeys).attach(error.to_string())
    })?;
    header.push(keyid_len);
    header.extend_from_slice(keyid);
    let mut padded = Vec::with_capacity(plaintext.len() + 1);
    padded.extend_from_slice(plaintext);
    padded.push(0x02);
    let cipher = Aes128Gcm::new(cek);
    // The additional data is a zero-length octet sequence (RFC 8188,
    // section 2): the header is transmitted in the clear and protects
    // itself through the record layout, not through the AEAD.
    let ciphertext = cipher
        .encrypt(
            nonce,
            AeadPayload {
                msg: &padded,
                aad: &[],
            },
        )
        .map_err(|error| {
            error_stack::Report::new(EncryptError::BadKeys).attach(error.to_string())
        })?;
    let mut body = header;
    body.extend_from_slice(&ciphertext);
    Ok(body)
}

/// How long a VAPID token stays valid: the RFC's maximum, less a margin so
/// a token minted just before a request is never refused as stale.
pub const TOKEN_LIFETIME: std::time::Duration = std::time::Duration::from_hours(12);

/// Signs `audience` with the instance's key, for the next `expires_in`
/// seconds, naming `contact` as the operator's address.
///
/// The VAPID header is a JWT with an ES256 signature: the push service
/// verifies it against the public key the subscription was made under, and
/// reads the audience to confirm the request is for its own origin. The
/// audience is the origin and nothing more — a path makes the service
/// refuse the token. The signing key is never the one the payload is sealed
/// with: the RFC requires the two to differ.
///
pub fn sign(
    audience: &str,
    signer: &SigningKey,
    contact: &str,
    expires_in: std::time::Duration,
) -> String {
    let expires_at = jiff::Timestamp::now().as_second()
        + i64::try_from(expires_in.as_secs()).unwrap_or(i64::MAX);
    let header = serde_json::json!({"typ": "JWT", "alg": "ES256"});
    let claims = serde_json::json!({"aud": audience, "exp": expires_at, "sub": contact});
    let signing_input = format!(
        "{}.{}",
        b64().encode(serde_json::to_vec(&header).unwrap_or_default()),
        b64().encode(serde_json::to_vec(&claims).unwrap_or_default())
    );
    // The JWS signature is the raw `r || s`, 64 octets — never the DER
    // form, which a push service refuses.
    let signature: Signature = signer.sign(signing_input.as_bytes());
    let sender_point = signer.verifying_key().to_encoded_point(false);
    format!(
        "vapid t={}.{}, k={}",
        signing_input,
        b64().encode(signature.to_bytes()),
        b64().encode(sender_point.as_bytes())
    )
}

/// The audience a push to `endpoint` is for: the push service's origin.
///
/// RFC 8292 requires it to be the origin and nothing more — a path makes the
/// service refuse the request.
#[must_use]
pub fn audience_of(endpoint: &str) -> Option<String> {
    let (scheme, rest) = endpoint.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    Some(format!("{scheme}://{authority}"))
}

/// The push service's answer to one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// The push service took it: it will wake the browser.
    Accepted,
    /// The subscription is gone — the browser unsubscribed, or the key it was
    /// made under no longer matches. Drop it.
    Gone,
    /// The service refused it for now: rate limited, or unreachable. Keep the
    /// subscription; nothing is retried.
    RetryLater,
}

/// Maps a push service's status code to what the sender does with the
/// subscription.
#[must_use]
pub fn outcome_of(status: u16) -> PushOutcome {
    match status {
        200..=201 => PushOutcome::Accepted,
        404 | 410 => PushOutcome::Gone,
        _ => PushOutcome::RetryLater,
    }
}

/// The five intermediates Appendix A prints, in order: `ecdh_secret`,
/// `prk_key`, `ikm`, `cek`, `nonce`. Five `Vec<u8>` in a tuple read as
/// nothing, so the stages get a name.
#[cfg(test)]
type Stages = (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>);

/// Every intermediate the RFC's Appendix A prints, in derivation order, so a
/// test can name the first stage that diverges from the published vector.
#[cfg(test)]
#[expect(clippy::expect_used, reason = "test code")]
fn derive_stages(
    ua_public: &p256::PublicKey,
    as_public: &p256::PublicKey,
    sender: &SigningKey,
    auth: &str,
    salt: &[u8; 16],
) -> Stages {
    let shared = p256::elliptic_curve::ecdh::diffie_hellman(
        sender.as_nonzero_scalar(),
        ua_public.as_affine(),
    );
    let ecdh = shared.raw_secret_bytes().to_vec();
    let auth_secret = b64().decode(auth).unwrap_or_default();
    let (prk_key, _) = Hkdf::<Sha256>::extract(Some(&auth_secret), ecdh.as_slice());
    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(ua_public.to_encoded_point(false).as_bytes());
    key_info.extend_from_slice(as_public.to_encoded_point(false).as_bytes());
    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::from_prk(&prk_key)
        .expect("a 32-byte PRK is a valid length")
        .expand(&key_info, &mut ikm)
        .expect("32 is a valid HKDF length");
    let (prk, _) = Hkdf::<Sha256>::extract(Some(salt), &ikm);
    let hk = Hkdf::<Sha256>::from_prk(&prk).expect("a 32-byte PRK is a valid length");
    let mut cek = [0u8; CEK_LEN];
    hk.expand(CEK_INFO, &mut cek)
        .expect("16 is a valid HKDF length");
    let mut nonce = [0u8; NONCE_LEN];
    hk.expand(NONCE_INFO, &mut nonce)
        .expect("12 is a valid HKDF length");
    (
        ecdh,
        prk_key.to_vec(),
        ikm.to_vec(),
        cek.to_vec(),
        nonce.to_vec(),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use p256::ecdsa::signature::Verifier as _;

    use super::{PushOutcome, Receiver, audience_of, outcome_of, sign};

    /// The RFC 8291 section 5 example, as the RFC prints it.
    const VECTOR: &str = concat!(
        "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27ml",
        "mlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPT",
        "pK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
    );

    fn vector_parts() -> (Receiver, p256::ecdsa::SigningKey, [u8; 16], &'static [u8]) {
        let receiver = Receiver {
            p256dh: concat!(
                "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcx",
                "aOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4"
            )
            .to_owned(),
            auth: "BTBZMqHH6r4Tts7J_aSIgg".to_owned(),
        };
        let sender = p256::ecdsa::SigningKey::from_slice(
            &base64::Engine::decode(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw",
            )
            .expect("the vector's sender key decodes"),
        )
        .expect("the vector's sender key is on the curve");
        let salt: [u8; 16] = base64::Engine::decode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            "DGv6ra1nlYgDCS1FRnbzlw",
        )
        .expect("the salt decodes")
        .try_into()
        .expect("the salt is 16 bytes");
        // The RFC's plaintext: 41 octets.
        let plaintext: &[u8] = b"When I grow up, I want to be a watermelon";
        (receiver, sender, salt, plaintext)
    }

    fn dec(s: &str) -> Vec<u8> {
        base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, s)
            .expect("decodes")
    }

    #[rstest::rstest]
    #[test]
    fn every_derivation_stage_matches_the_rfc_appendix() {
        // Given the RFC's receiver, sender, and salt.
        let (receiver, sender, salt, _) = vector_parts();
        let ua_public = p256::PublicKey::from_sec1_bytes(&dec(&receiver.p256dh)).expect("point");
        let as_public: p256::PublicKey = (*sender.verifying_key()).into();

        // When the stages are derived.
        let (ecdh, prk_key, ikm, cek, nonce) =
            super::derive_stages(&ua_public, &as_public, &sender, &receiver.auth, &salt);

        // Then each is the published one. A failure names the first stage
        // that diverges.
        assert_eq!(
            ecdh,
            dec("kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs"),
            "ecdh_secret"
        );
        assert_eq!(
            prk_key,
            dec("Snr3JMxaHVDXHWJn5wdC52WjpCtd2EIEGBykDcZW32k"),
            "PRK_key"
        );
        assert_eq!(
            ikm,
            dec("S4lYMb_L0FxCeq0WhDx813KgSYqU26kOyzWUdsXYyrg"),
            "IKM"
        );
        assert_eq!(cek, dec("oIhVW04MRdy2XN9CiKLxTg"), "CEK");
        assert_eq!(nonce, dec("4h_95klXJ5E_qnoN"), "NONCE");
    }

    #[rstest::rstest]
    #[test]
    fn the_record_matches_the_rfc_8291_vector_byte_for_byte() {
        // Given the RFC's receiver, sender, and salt.
        let (receiver, sender, salt, plaintext) = vector_parts();

        // When a record is sealed for the plaintext the RFC uses.
        let record = super::encrypt_bytes(plaintext, &receiver, &sender, &salt)
            .expect("the vector's keys are valid");

        // Then the body is the RFC's, byte for byte.
        let expected =
            base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, VECTOR)
                .expect("the vector decodes");
        assert_eq!(record.body, expected);
    }

    /// Splits a VAPID `Authorization` value into its token and key.
    fn split_auth(value: &str) -> (String, String) {
        let rest = value.strip_prefix("vapid t=").expect("the vapid scheme");
        let (token, key) = rest.split_once(", k=").expect("t and k");
        (token.to_owned(), key.to_owned())
    }

    /// Splits a JWT into its three dot-separated segments.
    fn split_jwt(token: &str) -> (&str, &str, &str) {
        let mut parts = token.splitn(3, '.');
        let header = parts.next().expect("header");
        let claims = parts.next().expect("claims");
        let signature = parts.next().expect("signature");
        (header, claims, signature)
    }

    /// Verifies a JWT against a verifying key, returning its claims.
    fn verify_jwt(token: &str, key: &p256::ecdsa::VerifyingKey) -> serde_json::Value {
        let (header, claims, sig) = split_jwt(token);
        let input = format!("{header}.{claims}");
        let raw = dec(sig);
        let signature = p256::ecdsa::Signature::from_slice(&raw).expect("a 64-byte signature");
        key.verify(input.as_bytes(), &signature)
            .expect("the signature verifies");
        serde_json::from_slice(&dec(claims)).expect("claims are JSON")
    }

    #[rstest::rstest]
    #[test]
    fn a_signed_token_verifies_against_the_published_key() {
        // Given a signing key.
        let signer = p256::ecdsa::SigningKey::from_slice(&[7u8; 32]).expect("on the curve");

        // When a token is signed.
        let authorization = sign(
            "https://push.example.net",
            &signer,
            "mailto:me@example.com",
            std::time::Duration::from_secs(60),
        );

        // Then it verifies against the `k` the header carries, and the
        // claims are the ones asked for.
        let (token, key) = split_auth(&authorization);
        let verifying = p256::ecdsa::VerifyingKey::from_sec1_bytes(&dec(&key)).expect("a point");
        let claims = verify_jwt(&token, &verifying);
        assert_eq!(
            claims.get("aud"),
            Some(&serde_json::json!("https://push.example.net"))
        );
        assert_eq!(
            claims.get("sub"),
            Some(&serde_json::json!("mailto:me@example.com"))
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_token_does_not_verify_against_another_key() {
        // Given a token signed by one key.
        let signer = p256::ecdsa::SigningKey::from_slice(&[7u8; 32]).expect("on the curve");
        let authorization = sign(
            "https://push.example.net",
            &signer,
            "mailto:me@example.com",
            std::time::Duration::from_secs(60),
        );
        let (token, _) = split_auth(&authorization);

        // When it is checked against a different key.
        let other = p256::ecdsa::VerifyingKey::from_sec1_bytes(
            p256::ecdsa::SigningKey::from_slice(&[9u8; 32])
                .expect("on the curve")
                .verifying_key()
                .to_encoded_point(false)
                .as_bytes(),
        )
        .expect("a point");

        // Then it does not verify.
        let (header, claims, sig) = split_jwt(&token);
        let signature = p256::ecdsa::Signature::from_slice(&dec(sig)).expect("a 64-byte signature");
        let input = format!("{header}.{claims}");
        assert!(other.verify(input.as_bytes(), &signature).is_err());
    }

    #[rstest::rstest]
    #[test]
    fn the_audience_is_the_origin_and_not_the_path() {
        // Given an endpoint on a vendor's push service.
        let endpoint = "https://fcm.googleapis.com/fcm/send/abc123";

        // When its audience is read.
        let audience = audience_of(endpoint);

        // Then it is the origin alone.
        assert_eq!(audience.as_deref(), Some("https://fcm.googleapis.com"));
    }

    #[rstest::rstest]
    #[test]
    fn an_endpoint_with_no_scheme_has_no_audience() {
        // Given an endpoint with no scheme.
        let endpoint = "fcm.googleapis.com/fcm/send/abc123";

        // When its audience is read.
        let audience = audience_of(endpoint);

        // Then there is none.
        assert!(audience.is_none());
    }

    #[rstest::rstest]
    #[case(201, PushOutcome::Accepted)]
    #[case(410, PushOutcome::Gone)]
    #[case(404, PushOutcome::Gone)]
    #[case(429, PushOutcome::RetryLater)]
    #[case(500, PushOutcome::RetryLater)]
    fn the_push_service_answer_maps_to_what_the_sender_does(
        #[case] status: u16,
        #[case] expected: PushOutcome,
    ) {
        // Given a status a push service answered with.
        // When it is mapped.
        let outcome = outcome_of(status);

        // Then it is the outcome that status means.
        assert_eq!(outcome, expected);
    }

    #[rstest::rstest]
    #[test]
    fn a_receiver_key_that_is_not_on_the_curve_is_refused() {
        // Given a receiver whose public key is not a point on P-256.
        let receiver = Receiver {
            p256dh: "AAAA".to_owned(),
            auth: "BTBZMqHH6r4Tts7J_aSIgg".to_owned(),
        };

        // When a record is sealed for it.
        let result = super::encrypt_bytes(
            b"anything",
            &receiver,
            &p256::ecdsa::SigningKey::from_slice(&[7u8; 32]).expect("seven is on the curve"),
            &[0u8; 16],
        );

        // Then it is refused.
        assert!(result.is_err());
    }
}
