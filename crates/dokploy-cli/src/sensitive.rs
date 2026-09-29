//! Per-instance key storage and opaque sensitive-intent fingerprinting.
//!
//! This module owns the complete secret-key lifecycle. Callers can load a
//! fingerprinter and calculate a durable receipt, but cannot read the key or
//! MAC through this module's interface.

use hmac::{Hmac, Mac};
use keyring::{Entry, Error as KeyringError};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::Zeroizing;

use dokploy_state::{
    FingerprintKeyId, InstanceIdentity, ResourceAddress, SensitiveFingerprint,
    SensitivePropertyPath,
};

const FINGERPRINT_DOMAIN: &[u8] = b"dokploy-iac\0sensitive-intent\0hmac-sha256-v1";
const KEY_ENVELOPE_MAGIC: &[u8; 8] = b"DOKHMAC1";
const KEY_ENVELOPE_LENGTH: usize = KEY_ENVELOPE_MAGIC.len() + 16 + 32;
const KEYRING_ACCOUNT_PREFIX: &str = "hmac-sha256-v1:";
const KEYRING_SERVICE: &str = "dokploy-sensitive-intent";

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum SensitiveFingerprintError {
    #[error("DOKSEC001: sensitive fingerprint key storage is unavailable")]
    StoreUnavailable,
    #[error("DOKSEC002: sensitive fingerprint key storage contains malformed data")]
    MalformedStoredKey,
    #[error("DOKSEC003: operating-system entropy is unavailable")]
    EntropyUnavailable,
}

trait FingerprintSecretStore {
    fn get(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>, SensitiveFingerprintError>;

    fn set(&self, account: &str, value: &[u8]) -> Result<(), SensitiveFingerprintError>;
}

trait FingerprintKeyGenerator {
    fn generate(&self) -> Result<FingerprintKey, SensitiveFingerprintError>;
}

#[derive(Clone, Copy, Debug, Default)]
struct KeyringFingerprintSecretStore;

impl FingerprintSecretStore for KeyringFingerprintSecretStore {
    fn get(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>, SensitiveFingerprintError> {
        let entry = keyring_entry(account)?;

        match entry.get_secret() {
            Ok(value) => Ok(Some(Zeroizing::new(value))),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(_) => Err(SensitiveFingerprintError::StoreUnavailable),
        }
    }

    fn set(&self, account: &str, value: &[u8]) -> Result<(), SensitiveFingerprintError> {
        keyring_entry(account)?
            .set_secret(value)
            .map_err(|_| SensitiveFingerprintError::StoreUnavailable)
    }
}

fn keyring_entry(account: &str) -> Result<Entry, SensitiveFingerprintError> {
    Entry::new(KEYRING_SERVICE, account).map_err(|_| SensitiveFingerprintError::StoreUnavailable)
}

#[derive(Clone, Copy, Debug, Default)]
struct SystemFingerprintKeyGenerator;

impl FingerprintKeyGenerator for SystemFingerprintKeyGenerator {
    fn generate(&self) -> Result<FingerprintKey, SensitiveFingerprintError> {
        let mut material = Zeroizing::new([0_u8; 48]);
        getrandom::fill(material.as_mut())
            .map_err(|_| SensitiveFingerprintError::EntropyUnavailable)?;

        let mut id_bytes: [u8; 16] = material[..16]
            .try_into()
            .expect("the key material ID slice has fixed length");
        id_bytes[6] = (id_bytes[6] & 0x0f) | 0x40;
        id_bytes[8] = (id_bytes[8] & 0x3f) | 0x80;
        let id = FingerprintKeyId::new(uuid::Uuid::from_bytes(id_bytes))
            .expect("a version-four UUID cannot be nil");
        let mut bytes = Zeroizing::new([0_u8; 32]);
        bytes.copy_from_slice(&material[16..]);

        Ok(FingerprintKey { id, bytes })
    }
}

struct FingerprintKey {
    id: FingerprintKeyId,
    bytes: Zeroizing<[u8; 32]>,
}

impl FingerprintKey {
    #[cfg(test)]
    fn fixture(id: &str, bytes: [u8; 32]) -> Self {
        Self {
            id: FingerprintKeyId::new(id.parse().expect("fixture key ID is a UUID"))
                .expect("fixture key ID is non-nil"),
            bytes: Zeroizing::new(bytes),
        }
    }

    fn decode(value: &[u8]) -> Result<Self, SensitiveFingerprintError> {
        if value.len() != KEY_ENVELOPE_LENGTH
            || &value[..KEY_ENVELOPE_MAGIC.len()] != KEY_ENVELOPE_MAGIC
        {
            return Err(SensitiveFingerprintError::MalformedStoredKey);
        }

        let id_start = KEY_ENVELOPE_MAGIC.len();
        let id_end = id_start + 16;
        let id = uuid::Uuid::from_slice(&value[id_start..id_end])
            .map_err(|_| SensitiveFingerprintError::MalformedStoredKey)?;
        let id =
            FingerprintKeyId::new(id).map_err(|_| SensitiveFingerprintError::MalformedStoredKey)?;
        let mut bytes = Zeroizing::new([0_u8; 32]);
        bytes.copy_from_slice(&value[id_end..]);

        Ok(Self { id, bytes })
    }

    fn encode(&self) -> Zeroizing<Vec<u8>> {
        let mut envelope = Zeroizing::new(Vec::with_capacity(KEY_ENVELOPE_LENGTH));
        envelope.extend_from_slice(KEY_ENVELOPE_MAGIC);
        envelope.extend_from_slice(self.id.as_uuid().as_bytes());
        envelope.extend_from_slice(self.bytes.as_ref());
        envelope
    }
}

impl std::fmt::Debug for FingerprintKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("FingerprintKey([REDACTED])")
    }
}

pub(crate) struct SensitiveFingerprinter {
    instance: InstanceIdentity,
    key: FingerprintKey,
}

impl SensitiveFingerprinter {
    pub(crate) fn load(instance: InstanceIdentity) -> Result<Self, SensitiveFingerprintError> {
        Self::load_with(
            instance,
            &KeyringFingerprintSecretStore,
            &SystemFingerprintKeyGenerator,
        )
    }

    fn from_key(instance: InstanceIdentity, key: FingerprintKey) -> Self {
        Self { instance, key }
    }

    fn load_with(
        instance: InstanceIdentity,
        store: &dyn FingerprintSecretStore,
        generator: &dyn FingerprintKeyGenerator,
    ) -> Result<Self, SensitiveFingerprintError> {
        let account = keyring_account(&instance);
        let stored = match store.get(&account)? {
            Some(stored) => stored,
            None => {
                let generated = generator.generate()?;
                let envelope = generated.encode();
                store.set(&account, envelope.as_slice())?;
                store
                    .get(&account)?
                    .ok_or(SensitiveFingerprintError::StoreUnavailable)?
            }
        };
        let key = FingerprintKey::decode(stored.as_slice())?;

        Ok(Self::from_key(instance, key))
    }

    #[cfg(test)]
    pub(crate) fn fixture(instance: InstanceIdentity, id: &str, bytes: [u8; 32]) -> Self {
        Self::from_key(instance, FingerprintKey::fixture(id, bytes))
    }

    pub(crate) fn fingerprint(
        &self,
        address: &ResourceAddress,
        path: &SensitivePropertyPath,
        value: &[u8],
    ) -> SensitiveFingerprint {
        let mut mac = Hmac::<Sha256>::new_from_slice(self.key.bytes.as_ref())
            .expect("HMAC-SHA-256 accepts keys of any length");
        mac.update(FINGERPRINT_DOMAIN);
        update_field(&mut mac, b"instance", self.instance.as_str().as_bytes());
        update_field(&mut mac, b"resource", address.to_string().as_bytes());
        update_field(&mut mac, b"property", path.to_string().as_bytes());
        update_field(&mut mac, b"value", value);
        let bytes: [u8; 32] = mac.finalize().into_bytes().into();

        SensitiveFingerprint::new_v1(self.key.id.clone(), bytes)
    }
}

fn keyring_account(instance: &InstanceIdentity) -> String {
    let digest = Sha256::digest(instance.as_str().as_bytes());
    format!("{KEYRING_ACCOUNT_PREFIX}{}", encode_hex(digest.as_slice()))
}

fn encode_hex(value: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len() * 2);

    for byte in value {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }

    encoded
}

impl std::fmt::Debug for SensitiveFingerprinter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SensitiveFingerprinter([REDACTED])")
    }
}

fn update_field(mac: &mut Hmac<Sha256>, label: &[u8], value: &[u8]) {
    let label_length = u64::try_from(label.len()).expect("field labels fit in u64");
    let value_length = u64::try_from(value.len()).expect("memory slices fit in u64");

    mac.update(&label_length.to_be_bytes());
    mac.update(label);
    mac.update(&value_length.to_be_bytes());
    mac.update(value);
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use serde_json::Value;
    use uuid::Uuid;
    use zeroize::Zeroizing;

    use dokploy_state::{InstanceIdentity, ResourceAddress, SensitivePropertyPath};

    use super::{
        FingerprintKey, FingerprintKeyGenerator, FingerprintSecretStore, SensitiveFingerprintError,
        SensitiveFingerprinter,
    };

    #[derive(Default)]
    struct MemorySecretStore {
        value: RefCell<Option<Vec<u8>>>,
        replacement_on_set: RefCell<Option<Vec<u8>>>,
        accounts: RefCell<Vec<String>>,
        gets: Cell<usize>,
        sets: Cell<usize>,
    }

    impl MemorySecretStore {
        fn with_value(value: Vec<u8>) -> Self {
            Self {
                value: RefCell::new(Some(value)),
                ..Self::default()
            }
        }
    }

    impl FingerprintSecretStore for MemorySecretStore {
        fn get(
            &self,
            account: &str,
        ) -> Result<Option<Zeroizing<Vec<u8>>>, SensitiveFingerprintError> {
            self.gets.set(self.gets.get() + 1);
            self.accounts.borrow_mut().push(account.to_owned());
            Ok(self.value.borrow().clone().map(Zeroizing::new))
        }

        fn set(&self, account: &str, value: &[u8]) -> Result<(), SensitiveFingerprintError> {
            self.sets.set(self.sets.get() + 1);
            self.accounts.borrow_mut().push(account.to_owned());
            let persisted = self
                .replacement_on_set
                .borrow_mut()
                .take()
                .unwrap_or_else(|| value.to_vec());
            *self.value.borrow_mut() = Some(persisted);
            Ok(())
        }
    }

    struct PanicGenerator;

    impl FingerprintKeyGenerator for PanicGenerator {
        fn generate(&self) -> Result<FingerprintKey, SensitiveFingerprintError> {
            panic!("an existing key must not consume entropy")
        }
    }

    struct FixedGenerator {
        id: &'static str,
        key: [u8; 32],
        calls: Cell<usize>,
    }

    impl FingerprintKeyGenerator for FixedGenerator {
        fn generate(&self) -> Result<FingerprintKey, SensitiveFingerprintError> {
            self.calls.set(self.calls.get() + 1);
            Ok(FingerprintKey::fixture(self.id, self.key))
        }
    }

    struct FailingStore;

    impl FingerprintSecretStore for FailingStore {
        fn get(
            &self,
            _account: &str,
        ) -> Result<Option<Zeroizing<Vec<u8>>>, SensitiveFingerprintError> {
            Err(SensitiveFingerprintError::StoreUnavailable)
        }

        fn set(&self, _account: &str, _value: &[u8]) -> Result<(), SensitiveFingerprintError> {
            Err(SensitiveFingerprintError::StoreUnavailable)
        }
    }

    struct SetFailingStore;

    impl FingerprintSecretStore for SetFailingStore {
        fn get(
            &self,
            _account: &str,
        ) -> Result<Option<Zeroizing<Vec<u8>>>, SensitiveFingerprintError> {
            Ok(None)
        }

        fn set(&self, _account: &str, _value: &[u8]) -> Result<(), SensitiveFingerprintError> {
            Err(SensitiveFingerprintError::StoreUnavailable)
        }
    }

    struct FailingGenerator;

    impl FingerprintKeyGenerator for FailingGenerator {
        fn generate(&self) -> Result<FingerprintKey, SensitiveFingerprintError> {
            Err(SensitiveFingerprintError::EntropyUnavailable)
        }
    }

    #[derive(Default)]
    struct VanishingStore {
        gets: Cell<usize>,
    }

    impl FingerprintSecretStore for VanishingStore {
        fn get(
            &self,
            _account: &str,
        ) -> Result<Option<Zeroizing<Vec<u8>>>, SensitiveFingerprintError> {
            self.gets.set(self.gets.get() + 1);
            Ok(None)
        }

        fn set(&self, _account: &str, _value: &[u8]) -> Result<(), SensitiveFingerprintError> {
            Ok(())
        }
    }

    fn envelope(id: &str, key: [u8; 32]) -> Vec<u8> {
        let mut envelope = b"DOKHMAC1".to_vec();
        envelope.extend_from_slice(Uuid::parse_str(id).unwrap().as_bytes());
        envelope.extend_from_slice(&key);
        envelope
    }

    #[test]
    fn fingerprinting_is_deterministic_for_one_sensitive_intent() {
        let instance = InstanceIdentity::parse("https://deploy.example.test").unwrap();
        let address: ResourceAddress = "redis.cache".parse().unwrap();
        let path = SensitivePropertyPath::parse("password").unwrap();
        let fingerprinter = SensitiveFingerprinter::from_key(
            instance,
            FingerprintKey::fixture("0199a0c8-2351-7c31-8899-2c8f81983ea5", [0x0b; 32]),
        );

        let first = fingerprinter.fingerprint(&address, &path, b"sensitive-value");
        let second = fingerprinter.fingerprint(&address, &path, b"sensitive-value");

        assert_eq!(first, second);
    }

    #[test]
    fn fingerprinting_matches_the_canonical_hmac_sha256_vector() {
        let instance = InstanceIdentity::parse("https://deploy.example.test").unwrap();
        let address: ResourceAddress = "redis.cache".parse().unwrap();
        let path = SensitivePropertyPath::parse("password").unwrap();
        let fingerprinter = SensitiveFingerprinter::from_key(
            instance,
            FingerprintKey::fixture("0199a0c8-2351-7c31-8899-2c8f81983ea5", [0x0b; 32]),
        );

        let fingerprint = fingerprinter.fingerprint(&address, &path, b"sensitive-value");
        let serialized = serde_json::to_value(fingerprint).unwrap();

        assert_eq!(
            serialized["mac"],
            Value::String(
                "f87cd7689eaed1dceb4215946b8b6a245806ea34400f1464596549865e1c96a7".into()
            )
        );
    }

    #[test]
    fn every_fingerprint_domain_dimension_changes_the_receipt() {
        let key_id = "0199a0c8-2351-7c31-8899-2c8f81983ea5";
        let instance = InstanceIdentity::parse("https://deploy.example.test").unwrap();
        let other_instance = InstanceIdentity::parse("https://other.example.test").unwrap();
        let address: ResourceAddress = "application.api".parse().unwrap();
        let other_address: ResourceAddress = "application.worker".parse().unwrap();
        let path = SensitivePropertyPath::parse("environment.TOKEN").unwrap();
        let other_path = SensitivePropertyPath::parse("environment.API_KEY").unwrap();
        let fingerprinter = SensitiveFingerprinter::from_key(
            instance.clone(),
            FingerprintKey::fixture(key_id, [7; 32]),
        );
        let baseline = fingerprinter.fingerprint(&address, &path, b"value");

        let changed_instance = SensitiveFingerprinter::from_key(
            other_instance,
            FingerprintKey::fixture(key_id, [7; 32]),
        )
        .fingerprint(&address, &path, b"value");
        let changed_address = fingerprinter.fingerprint(&other_address, &path, b"value");
        let changed_path = fingerprinter.fingerprint(&address, &other_path, b"value");
        let changed_value = fingerprinter.fingerprint(&address, &path, b"other");
        let changed_key =
            SensitiveFingerprinter::from_key(instance, FingerprintKey::fixture(key_id, [8; 32]))
                .fingerprint(&address, &path, b"value");

        for changed in [
            changed_instance,
            changed_address,
            changed_path,
            changed_value,
            changed_key,
        ] {
            assert_ne!(baseline, changed);
        }
    }

    #[test]
    fn changing_only_the_key_identifier_rotates_the_receipt() {
        let instance = InstanceIdentity::parse("https://deploy.example.test").unwrap();
        let address: ResourceAddress = "redis.cache".parse().unwrap();
        let path = SensitivePropertyPath::parse("password").unwrap();
        let before = SensitiveFingerprinter::from_key(
            instance.clone(),
            FingerprintKey::fixture("0199a0c8-2351-7c31-8899-2c8f81983ea5", [7; 32]),
        )
        .fingerprint(&address, &path, b"value");
        let after = SensitiveFingerprinter::from_key(
            instance,
            FingerprintKey::fixture("0199a0c8-2351-7c31-8899-2c8f81983ea6", [7; 32]),
        )
        .fingerprint(&address, &path, b"value");

        assert_ne!(before, after);
    }

    #[test]
    fn existing_per_instance_key_is_loaded_without_generating_or_storing() {
        let key_id = "0199a0c8-2351-7c31-8899-2c8f81983ea5";
        let store = MemorySecretStore::with_value(envelope(key_id, [9; 32]));
        let instance = InstanceIdentity::parse("https://deploy.example.test").unwrap();

        let loaded = SensitiveFingerprinter::load_with(instance.clone(), &store, &PanicGenerator)
            .expect("existing key loads");
        let expected =
            SensitiveFingerprinter::from_key(instance, FingerprintKey::fixture(key_id, [9; 32]));
        let address: ResourceAddress = "redis.cache".parse().unwrap();
        let path = SensitivePropertyPath::parse("password").unwrap();

        assert_eq!(
            loaded.fingerprint(&address, &path, b"value"),
            expected.fingerprint(&address, &path, b"value")
        );
        assert_eq!(store.gets.get(), 1);
        assert_eq!(store.sets.get(), 0);
    }

    #[test]
    fn missing_key_is_generated_stored_and_reread_before_use() {
        let generated_id = "0199a0c8-2351-7c31-8899-2c8f81983ea5";
        let persisted_id = "0199a0c8-2351-7c31-8899-2c8f81983ea6";
        let store = MemorySecretStore {
            replacement_on_set: RefCell::new(Some(envelope(persisted_id, [12; 32]))),
            ..MemorySecretStore::default()
        };
        let generator = FixedGenerator {
            id: generated_id,
            key: [11; 32],
            calls: Cell::new(0),
        };
        let instance = InstanceIdentity::parse("https://deploy.example.test").unwrap();

        let loaded = SensitiveFingerprinter::load_with(instance.clone(), &store, &generator)
            .expect("missing key is initialized");
        let persisted = SensitiveFingerprinter::from_key(
            instance,
            FingerprintKey::fixture(persisted_id, [12; 32]),
        );
        let address: ResourceAddress = "redis.cache".parse().unwrap();
        let path = SensitivePropertyPath::parse("password").unwrap();

        assert_eq!(
            loaded.fingerprint(&address, &path, b"value"),
            persisted.fingerprint(&address, &path, b"value")
        );
        assert_eq!(generator.calls.get(), 1);
        assert_eq!(store.gets.get(), 2);
        assert_eq!(store.sets.get(), 1);
    }

    #[test]
    fn keyring_account_is_stable_bounded_and_does_not_expose_the_instance() {
        let key_id = "0199a0c8-2351-7c31-8899-2c8f81983ea5";
        let store = MemorySecretStore::with_value(envelope(key_id, [9; 32]));
        let raw_instance = "https://deploy.example.test/api";
        let instance = InstanceIdentity::parse(raw_instance).unwrap();

        SensitiveFingerprinter::load_with(instance, &store, &PanicGenerator).unwrap();

        let accounts = store.accounts.borrow();
        assert_eq!(accounts.len(), 1);
        assert!(accounts[0].starts_with("hmac-sha256-v1:"));
        assert_eq!(accounts[0].len(), "hmac-sha256-v1:".len() + 64);
        assert!(!accounts[0].contains("deploy.example.test"));
        assert!(!accounts[0].contains(raw_instance));
        assert_eq!(
            accounts[0],
            super::keyring_account(
                &InstanceIdentity::parse("https://deploy.example.test/").unwrap()
            )
        );
    }

    #[test]
    fn malformed_or_nil_stored_keys_fail_closed() {
        let instance = InstanceIdentity::parse("https://deploy.example.test").unwrap();
        let mut nil_id = b"DOKHMAC1".to_vec();
        nil_id.extend_from_slice(&[0; 16]);
        nil_id.extend_from_slice(&[7; 32]);
        let malformed = [
            b"short".to_vec(),
            envelope("0199a0c8-2351-7c31-8899-2c8f81983ea5", [7; 32])
                .into_iter()
                .chain([0])
                .collect(),
            {
                let mut wrong_magic = envelope("0199a0c8-2351-7c31-8899-2c8f81983ea5", [7; 32]);
                wrong_magic[0] = b'X';
                wrong_magic
            },
            nil_id,
        ];

        for value in malformed {
            let store = MemorySecretStore::with_value(value);
            let error =
                SensitiveFingerprinter::load_with(instance.clone(), &store, &PanicGenerator)
                    .expect_err("malformed key must be rejected");

            assert_eq!(error, SensitiveFingerprintError::MalformedStoredKey);
            assert_eq!(store.sets.get(), 0);
        }
    }

    #[test]
    fn storage_entropy_and_reread_failures_never_fall_back() {
        let instance = InstanceIdentity::parse("https://deploy.example.test").unwrap();
        let generator = FixedGenerator {
            id: "0199a0c8-2351-7c31-8899-2c8f81983ea5",
            key: [11; 32],
            calls: Cell::new(0),
        };

        let access = SensitiveFingerprinter::load_with(instance.clone(), &FailingStore, &generator)
            .expect_err("storage failure is fatal");
        let set = SensitiveFingerprinter::load_with(instance.clone(), &SetFailingStore, &generator)
            .expect_err("storage write failure is fatal");
        let entropy = SensitiveFingerprinter::load_with(
            instance.clone(),
            &MemorySecretStore::default(),
            &FailingGenerator,
        )
        .expect_err("entropy failure is fatal");
        let vanished =
            SensitiveFingerprinter::load_with(instance, &VanishingStore::default(), &generator)
                .expect_err("a key missing after storage is fatal");

        assert_eq!(access, SensitiveFingerprintError::StoreUnavailable);
        assert_eq!(set, SensitiveFingerprintError::StoreUnavailable);
        assert_eq!(entropy, SensitiveFingerprintError::EntropyUnavailable);
        assert_eq!(vanished, SensitiveFingerprintError::StoreUnavailable);
    }

    #[test]
    fn debug_and_errors_never_expose_key_or_value_canaries() {
        let key_canary = "fingerprint-key-canary";
        let value_canary = "sensitive-value-canary";
        let mut key = [0_u8; 32];
        key[..key_canary.len()].copy_from_slice(key_canary.as_bytes());
        let instance = InstanceIdentity::parse("https://deploy.example.test").unwrap();
        let fingerprinter = SensitiveFingerprinter::from_key(
            instance,
            FingerprintKey::fixture("0199a0c8-2351-7c31-8899-2c8f81983ea5", key),
        );
        let address: ResourceAddress = "redis.cache".parse().unwrap();
        let path = SensitivePropertyPath::parse("password").unwrap();
        let receipt = fingerprinter.fingerprint(&address, &path, value_canary.as_bytes());
        let output = format!(
            "{fingerprinter:?} {receipt:?} {:?} {}",
            SensitiveFingerprintError::MalformedStoredKey,
            SensitiveFingerprintError::MalformedStoredKey,
        );

        assert!(!output.contains(key_canary));
        assert!(!output.contains(value_canary));
        assert!(output.contains("[REDACTED]"));
    }
}
