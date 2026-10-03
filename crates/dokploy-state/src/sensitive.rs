use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser::SerializeStruct};
use thiserror::Error;
use uuid::Uuid;

const FINGERPRINT_VERSION: &str = "hmac-sha256-v1";
const SHA256_MAC_LENGTH: usize = 32;

/// The stable, non-nil identifier of one sensitive-intent fingerprint key.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FingerprintKeyId(Uuid);

impl FingerprintKeyId {
    /// Validates a fingerprint key identifier.
    pub fn new(value: Uuid) -> Result<Self, FingerprintKeyIdError> {
        if value.is_nil() {
            return Err(FingerprintKeyIdError);
        }

        Ok(Self(value))
    }

    /// Returns the UUID for key lookup without exposing fingerprint bytes.
    #[must_use]
    pub const fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl fmt::Debug for FingerprintKeyId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FingerprintKeyId([REDACTED])")
    }
}

impl Serialize for FingerprintKeyId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.hyphenated().to_string())
    }
}

impl<'de> Deserialize<'de> for FingerprintKeyId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let parsed =
            Uuid::parse_str(&value).map_err(|_| de::Error::custom(FingerprintKeyIdError))?;
        let key_id = Self::new(parsed).map_err(de::Error::custom)?;
        if value != key_id.0.hyphenated().to_string() {
            return Err(de::Error::custom(FingerprintKeyIdError));
        }

        Ok(key_id)
    }
}

/// A nil or noncanonical sensitive-intent fingerprint key identifier.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("fingerprint key ID must be a non-nil canonical UUID")]
pub struct FingerprintKeyIdError;

/// A versioned HMAC-SHA-256 receipt for one sensitive input intent.
///
/// The receipt is deliberately opaque: callers can identify the key version,
/// but cannot read the stored MAC through the Rust interface or debug output.
#[derive(Clone, Eq, PartialEq)]
pub struct SensitiveFingerprint {
    key_id: FingerprintKeyId,
    mac: [u8; SHA256_MAC_LENGTH],
}

impl SensitiveFingerprint {
    /// Creates a version-one HMAC-SHA-256 intent receipt.
    #[must_use]
    pub const fn new_v1(key_id: FingerprintKeyId, mac: [u8; SHA256_MAC_LENGTH]) -> Self {
        Self { key_id, mac }
    }

    /// Returns the identifier needed to select the verification key.
    #[must_use]
    pub const fn key_id(&self) -> &FingerprintKeyId {
        &self.key_id
    }
}

impl fmt::Debug for SensitiveFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SensitiveFingerprint([REDACTED])")
    }
}

impl Serialize for SensitiveFingerprint {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut receipt = serializer.serialize_struct("SensitiveFingerprint", 3)?;
        receipt.serialize_field("version", FINGERPRINT_VERSION)?;
        receipt.serialize_field("keyId", &self.key_id)?;
        receipt.serialize_field("mac", &encode_mac(&self.mac))?;
        receipt.end()
    }
}

impl<'de> Deserialize<'de> for SensitiveFingerprint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        #[serde(rename_all = "camelCase")]
        struct Receipt {
            version: String,
            key_id: FingerprintKeyId,
            mac: String,
        }

        let receipt = Receipt::deserialize(deserializer)?;
        if receipt.version != FINGERPRINT_VERSION {
            return Err(de::Error::custom(SensitiveFingerprintDecodeError));
        }
        let mac = decode_mac(&receipt.mac)
            .ok_or_else(|| de::Error::custom(SensitiveFingerprintDecodeError))?;

        Ok(Self::new_v1(receipt.key_id, mac))
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("sensitive fingerprint receipt is malformed or noncanonical")]
struct SensitiveFingerprintDecodeError;

fn encode_mac(mac: &[u8; SHA256_MAC_LENGTH]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(SHA256_MAC_LENGTH * 2);
    for byte in mac {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn decode_mac(value: &str) -> Option<[u8; SHA256_MAC_LENGTH]> {
    if value.len() != SHA256_MAC_LENGTH * 2 {
        return None;
    }

    let mut decoded = [0_u8; SHA256_MAC_LENGTH];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = decode_nibble(pair[0])?;
        let low = decode_nibble(pair[1])?;
        decoded[index] = (high << 4) | low;
    }

    Some(decoded)
}

fn decode_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

/// The durable path of one sensitive property: a dotted path such as `password` or
/// `environment.LOG_LEVEL`.
///
/// State checks only the syntax. Which paths are legal for a kind, and which are
/// sensitive, is decided by the kind's spec (ADR 0004) in the layers that read specs.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SensitivePropertyPath(String);

const MAX_SENSITIVE_PATH_LEN: usize = 256;
const MAX_SENSITIVE_PATH_SEGMENTS: usize = 4;

impl SensitivePropertyPath {
    /// Parses a dotted property path.
    ///
    /// The first segment is a lower snake case field name. Later segments are
    /// collection keys or struct members: letters, digits, and underscores, not
    /// starting with a digit.
    pub fn parse(value: &str) -> Result<Self, SensitivePropertyPathError> {
        if value.len() > MAX_SENSITIVE_PATH_LEN {
            return Err(SensitivePropertyPathError);
        }
        let mut segments = value.split('.');
        let first = segments.next().ok_or(SensitivePropertyPathError)?;
        let mut count = 1;
        let first_ok = {
            let mut characters = first.chars();
            characters.next().is_some_and(|c| c.is_ascii_lowercase())
                && characters.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        };
        if !first_ok {
            return Err(SensitivePropertyPathError);
        }
        for segment in segments {
            count += 1;
            if count > MAX_SENSITIVE_PATH_SEGMENTS || !valid_environment_name(segment) {
                return Err(SensitivePropertyPathError);
            }
        }

        Ok(Self(value.to_owned()))
    }

    /// Returns the dotted path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the path's segments, outermost first.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('.')
    }
}

impl fmt::Display for SensitivePropertyPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl fmt::Debug for SensitivePropertyPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_string())
    }
}

impl Serialize for SensitivePropertyPath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SensitivePropertyPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

/// A path that is not a valid dotted property path.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error(
    "sensitive input path must be a lower snake case field optionally followed by up to three keys, such as `password` or `environment.NAME`"
)]
pub struct SensitivePropertyPathError;

/// Durable sensitive intent receipts in canonical property-path order.
#[derive(Clone, Default, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SensitiveInputs(BTreeMap<SensitivePropertyPath, SensitiveFingerprint>);

impl SensitiveInputs {
    /// Collects unique, validated sensitive-property receipts.
    pub fn try_from_entries(
        entries: impl IntoIterator<Item = (SensitivePropertyPath, SensitiveFingerprint)>,
    ) -> Result<Self, SensitiveInputsError> {
        let mut inputs = BTreeMap::new();
        for (path, fingerprint) in entries {
            if inputs.insert(path.clone(), fingerprint).is_some() {
                return Err(SensitiveInputsError::DuplicatePath { path });
            }
        }

        Ok(Self(inputs))
    }

    /// Returns the opaque receipt associated with one sensitive property.
    #[must_use]
    pub fn fingerprint(&self, path: &SensitivePropertyPath) -> Option<&SensitiveFingerprint> {
        self.0.get(path)
    }

    /// Iterates over canonical paths without exposing fingerprint bytes.
    pub fn paths(&self) -> impl Iterator<Item = &SensitivePropertyPath> {
        self.0.keys()
    }

    /// Returns whether no sensitive properties are owned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SensitiveInputs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SensitiveInputs")
            .field("property_count", &self.0.len())
            .finish()
    }
}

impl<'de> Deserialize<'de> for SensitiveInputs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let inputs =
            BTreeMap::<SensitivePropertyPath, SensitiveFingerprint>::deserialize(deserializer)?;
        Ok(Self(inputs))
    }
}

/// Sensitive input entries that cannot form one canonical durable map.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SensitiveInputsError {
    /// The same property path was supplied more than once.
    #[error("sensitive input path `{path}` is duplicated")]
    DuplicatePath { path: SensitivePropertyPath },
}

/// A collection key or struct member: letters, digits, and underscores, not starting with
/// a digit. Environment variable names use the same rule.
pub(crate) fn valid_environment_name(value: &str) -> bool {
    if value.len() > 256 {
        return false;
    }

    let mut characters = value.chars();
    matches!(characters.next(), Some(first) if first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}
