//! The instance's push key pair: the P-256 pair every push is signed with.
//!
//! Generated once, the first time a push needs it, and kept for the life of
//! the installation. It is the instance's identity to every push service —
//! each browser's service verifies each request against the public half, and
//! no service requires the pair be registered anywhere. Losing the private
//! half orphans every subscription made under it: the push service then
//! answers a push with `403`, that subscription is dropped, and the page
//! subscribes again on its next load with the new key.

use p256::ecdsa::SigningKey;
use p256::elliptic_curve::scalar::NonZeroScalar;

/// The uncompressed P-256 point, as the browser's `applicationServerKey`
/// and the VAPID header's `k` parameter take it: unpadded base64url.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey(String);

impl PublicKey {
    /// The key as unpadded base64url — the wire form the page subscribes with.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A key pair: the signing half the server keeps, and the public half the
/// page subscribes with.
#[derive(Clone)]
pub struct KeyPair {
    /// The private signing key. Never serialized to the wire and never logged.
    signing: SigningKey,
    /// The public half, in its wire form.
    public: PublicKey,
}

impl std::fmt::Debug for KeyPair {
    /// Names only the public half: the private key must never reach a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyPair")
            .field("public", &self.public)
            .finish_non_exhaustive()
    }
}

impl KeyPair {
    /// Generates a fresh pair from the OS RNG.
    ///
    /// `SigningKey::random` draws a non-zero scalar below the curve's
    /// order and redraws anything else, so it always returns a usable key
    /// and there is no failure to carry. The RNG is `rand_core 0.6`'s
    /// `OsRng`, the version `p256` is built on; the workspace's `rand 0.9`
    /// sits on a later `rand_core` and does not satisfy the bound.
    #[must_use]
    pub fn generate() -> Self {
        use rand_core06::OsRng;
        Self::from_signing(SigningKey::random(&mut OsRng))
    }

    /// Rebuilds the pair from a stored private key.
    ///
    /// `stored` is the raw 32-byte P-256 scalar, base64url-encoded as the
    /// file keeps it. `None` when it is not a scalar on the curve — a corrupt
    /// file, which the caller answers by generating a fresh pair.
    #[must_use]
    pub fn from_stored(stored: &str) -> Option<Self> {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(stored)
            .ok()?;
        let fixed: [u8; 32] = bytes.try_into().ok()?;
        let scalar = NonZeroScalar::from_repr(fixed.into()).into_option()?;
        Some(Self::from_signing(SigningKey::from(scalar)))
    }

    /// The private scalar in its stored form: raw 32 bytes, base64url.
    #[must_use]
    pub fn stored_form(&self) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.signing.to_bytes())
    }

    /// The public half, in its wire form.
    #[must_use]
    pub fn public_key(&self) -> &PublicKey {
        &self.public
    }

    /// The signing half, for the VAPID signature.
    pub(crate) fn signing(&self) -> &SigningKey {
        &self.signing
    }

    fn from_signing(signing: SigningKey) -> Self {
        use base64::Engine as _;
        // The uncompressed point: 0x04 || x || y, 65 bytes.
        let point = signing.verifying_key().to_encoded_point(false);
        Self {
            signing,
            public: PublicKey(
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(point.as_bytes()),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use super::KeyPair;

    #[rstest::rstest]
    #[test]
    fn a_generated_public_key_is_the_uncompressed_point() {
        // Given a freshly generated pair.
        let pair = KeyPair::generate();

        // Then the public key is 65 bytes as unpadded base64url: 87 chars.
        assert_eq!(pair.public_key().as_str().len(), 87);
    }

    #[rstest::rstest]
    #[test]
    fn a_pair_rebuilt_from_its_stored_form_has_the_same_public_key() {
        // Given a pair, and its private half in stored form.
        let pair = KeyPair::generate();
        let stored = pair.stored_form();

        // When a pair is rebuilt from that form.
        let rebuilt = KeyPair::from_stored(&stored).expect("the stored form is a scalar");

        // Then its public key is the original's.
        assert_eq!(rebuilt.public_key(), pair.public_key());
    }

    #[rstest::rstest]
    #[test]
    fn a_stored_form_that_is_not_on_the_curve_is_rejected() {
        // Given a stored form that is not a valid base64url scalar.
        let stored = "not-a-key";

        // When a pair is rebuilt from it.
        let rebuilt = KeyPair::from_stored(stored);

        // Then there is none.
        assert!(rebuilt.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn the_zero_scalar_is_rejected() {
        // Given the zero scalar — valid base64url, not on the curve.
        use base64::Engine as _;
        let stored = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0u8; 32]);

        // When a pair is rebuilt from it.
        let rebuilt = KeyPair::from_stored(&stored);

        // Then there is none.
        assert!(rebuilt.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn two_generated_pairs_differ() {
        // Given two freshly generated pairs.
        let first = KeyPair::generate();
        let second = KeyPair::generate();

        // Then their public keys differ.
        assert_ne!(first.public_key(), second.public_key());
    }

    #[rstest::rstest]
    #[test]
    fn the_debug_form_names_no_private_material() {
        // Given a pair.
        let pair = KeyPair::generate();

        // When it is rendered for a log.
        let rendered = format!("{pair:?}");

        // Then the private half's stored form is nowhere in it.
        assert!(!rendered.contains(&pair.stored_form()));
    }
}
