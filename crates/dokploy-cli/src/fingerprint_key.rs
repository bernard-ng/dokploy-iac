//! The key that fingerprints secrets (ADR 0010).
//!
//! `DOKPLOY_FINGERPRINT_KEY` (`<uuid>:<64 hex>`) wins, which is what CI sets. Otherwise a key
//! per instance lives in the operating system's credential store, and is generated on first use.

use dokploy_engine::FingerprintKey;
use dokploy_state::InstanceIdentity;
use keyring::Entry;
use miette::{IntoDiagnostic, Result};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

const SERVICE: &str = "dokploy-iac";
const ENVIRONMENT: &str = "DOKPLOY_FINGERPRINT_KEY";

/// Loads the key for `instance`.
pub fn load(instance: &InstanceIdentity) -> Result<FingerprintKey> {
    if let Some(value) = std::env::var_os(ENVIRONMENT) {
        let value = Zeroizing::new(value.into_string().map_err(|_| malformed())?);
        return FingerprintKey::parse_explicit(&value).map_err(|_| malformed());
    }

    let digest = Sha256::digest(instance.as_str().as_bytes());
    let account = format!("fingerprint-key-{}", hex(digest.as_slice()));
    let entry = Entry::new(SERVICE, &account).into_diagnostic()?;
    let stored = match entry.get_password() {
        Ok(stored) => Zeroizing::new(stored),
        Err(keyring::Error::NoEntry) => {
            let mut bytes = Zeroizing::new([0_u8; 32]);
            getrandom::fill(bytes.as_mut())
                .map_err(|error| miette::miette!("no entropy: {error}"))?;
            let generated = Zeroizing::new(format!("{}:{}", Uuid::new_v4(), hex(bytes.as_ref())));
            entry.set_password(&generated).into_diagnostic()?;
            generated
        }
        Err(error) => return Err(error).into_diagnostic(),
    };

    FingerprintKey::parse_explicit(&stored).map_err(|_| {
        miette::miette!(
            "the stored fingerprint key is malformed; remove the `{SERVICE}` credential `{account}`"
        )
    })
}

fn malformed() -> miette::Report {
    miette::miette!("{ENVIRONMENT} must be `<uuid>:<64 lowercase hexadecimal characters>`")
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }

    text
}
