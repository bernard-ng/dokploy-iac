//! Keyed fingerprints of secret inputs.
//!
//! A fingerprint is an HMAC-SHA-256 of a value under a per-instance key, bound to the
//! instance, the resource address, and the property path, so the same secret has a
//! different receipt anywhere else. State holds receipts, never values (invariant 18).
//! The construction is the first engine's, so receipts stay comparable.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroizing;

use dokploy_state::{
    FingerprintKeyId, InstanceIdentity, ResourceAddress, SensitiveFingerprint,
    SensitivePropertyPath,
};

const DOMAIN: &[u8] = b"dokploy-iac\0sensitive-intent\0hmac-sha256-v1";

/// A fingerprint key: an identifier and 32 secret bytes.
pub struct FingerprintKey {
    id: FingerprintKeyId,
    bytes: Zeroizing<[u8; 32]>,
}

impl FingerprintKey {
    /// Builds a key from its parts.
    #[must_use]
    pub fn new(id: FingerprintKeyId, bytes: [u8; 32]) -> Self {
        Self {
            id,
            bytes: Zeroizing::new(bytes),
        }
    }

    /// Parses `<non-nil uuid>:<64 lowercase hex>`, the form of `DOKPLOY_FINGERPRINT_KEY`.
    pub fn parse_explicit(value: &str) -> Result<Self, FingerprintKeyError> {
        let (id, hex) = value.split_once(':').ok_or(FingerprintKeyError)?;
        if hex.len() != 64 || hex.contains(':') {
            return Err(FingerprintKeyError);
        }
        let id = id.parse::<Uuid>().map_err(|_| FingerprintKeyError)?;
        let id = FingerprintKeyId::new(id).map_err(|_| FingerprintKeyError)?;
        let mut bytes = Zeroizing::new([0_u8; 32]);
        for (index, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
            bytes[index] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }

        Ok(Self { id, bytes })
    }
}

fn nibble(value: u8) -> Result<u8, FingerprintKeyError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(FingerprintKeyError),
    }
}

impl std::fmt::Debug for FingerprintKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("FingerprintKey([REDACTED])")
    }
}

/// A malformed explicit fingerprint key.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("a fingerprint key is `<uuid>:<64 lowercase hex characters>`")]
pub struct FingerprintKeyError;

/// Computes receipts for one instance.
pub struct Fingerprinter {
    instance: InstanceIdentity,
    key: FingerprintKey,
}

impl Fingerprinter {
    /// Binds a key to an instance.
    #[must_use]
    pub fn new(instance: InstanceIdentity, key: FingerprintKey) -> Self {
        Self { instance, key }
    }

    /// The receipt of `value` for one property of one resource.
    #[must_use]
    pub fn fingerprint(
        &self,
        address: &ResourceAddress,
        path: &SensitivePropertyPath,
        value: &[u8],
    ) -> SensitiveFingerprint {
        let mut mac = Hmac::<Sha256>::new_from_slice(self.key.bytes.as_ref())
            .expect("HMAC-SHA-256 accepts keys of any length");
        mac.update(DOMAIN);
        field(&mut mac, b"instance", self.instance.as_str().as_bytes());
        field(&mut mac, b"resource", address.to_string().as_bytes());
        field(&mut mac, b"property", path.to_string().as_bytes());
        field(&mut mac, b"value", value);
        let bytes: [u8; 32] = mac.finalize().into_bytes().into();

        SensitiveFingerprint::new_v1(self.key.id.clone(), bytes)
    }
}

impl std::fmt::Debug for Fingerprinter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Fingerprinter([REDACTED])")
    }
}

fn field(mac: &mut Hmac<Sha256>, label: &[u8], value: &[u8]) {
    let label_length = u64::try_from(label.len()).expect("labels fit in u64");
    let value_length = u64::try_from(value.len()).expect("memory slices fit in u64");
    mac.update(&label_length.to_be_bytes());
    mac.update(label);
    mac.update(&value_length.to_be_bytes());
    mac.update(value);
}
