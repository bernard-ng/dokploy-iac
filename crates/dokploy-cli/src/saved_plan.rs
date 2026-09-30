//! Strict, redaction-safe saved plan envelopes for deferred apply.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use dokploy_core::{ConfigDigest, Plan};
use dokploy_state::InstanceIdentity;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::strict_json::reject_duplicate_keys;

const FORMAT_VERSION: u32 = 1;
const MAX_SAVED_PLAN_BYTES: u64 = 4 * 1024 * 1024;

/// A persisted public plan plus the evidence required to rebind a fresh plan.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedPlan {
    format_version: u32,
    instance: InstanceIdentity,
    lineage: Uuid,
    state_serial: u64,
    config_digest: String,
    plan_digest: String,
    remote_receipt: String,
    plan: serde_json::Value,
}

impl SavedPlan {
    /// Captures one complete, applyable public plan and its keyed remote receipt.
    pub fn from_fresh_plan(
        instance: InstanceIdentity,
        plan: &Plan,
        remote_receipt: [u8; 32],
    ) -> Result<Self, SavedPlanError> {
        if !plan.complete() || !plan.applyable() {
            return Err(SavedPlanError::PlanBlocked);
        }
        let plan: serde_json::Value = serde_json::from_slice(&plan.to_json_bytes())
            .map_err(|_| SavedPlanError::InvalidDocument)?;
        let canonical = serde_json::to_vec(&plan).map_err(|_| SavedPlanError::InvalidDocument)?;

        Ok(Self {
            format_version: FORMAT_VERSION,
            instance,
            lineage: plan_uuid(&plan, "lineage")?,
            state_serial: plan_u64(&plan, "stateSerial")?,
            config_digest: plan_string(&plan, "configDigest")?.to_owned(),
            plan_digest: encode_hex(Sha256::digest(&canonical).as_slice()),
            remote_receipt: encode_hex(&remote_receipt),
            plan,
        })
    }

    /// Returns the bound Dokploy instance without exposing remote identities.
    #[must_use]
    pub const fn instance(&self) -> &InstanceIdentity {
        &self.instance
    }

    /// Returns the state lineage used for the saved plan.
    #[must_use]
    pub const fn lineage(&self) -> Uuid {
        self.lineage
    }

    /// Returns the state serial used for the saved plan.
    #[must_use]
    pub const fn state_serial(&self) -> u64 {
        self.state_serial
    }

    /// Verifies a freshly reconstructed plan and keyed remote observation receipt.
    pub fn verify_fresh(
        &self,
        instance: &InstanceIdentity,
        plan: &Plan,
        remote_receipt: [u8; 32],
    ) -> Result<(), SavedPlanError> {
        if &self.instance != instance
            || self.lineage != plan.lineage()
            || self.state_serial != plan.state_serial()
            || self.config_digest != plan.config_digest().as_str()
        {
            return Err(SavedPlanError::StaleEvidence);
        }
        let fresh: serde_json::Value = serde_json::from_slice(&plan.to_json_bytes())
            .map_err(|_| SavedPlanError::InvalidDocument)?;
        let canonical = serde_json::to_vec(&fresh).map_err(|_| SavedPlanError::InvalidDocument)?;
        if self.plan != fresh
            || self.plan_digest != encode_hex(Sha256::digest(&canonical).as_slice())
            || self.remote_receipt != encode_hex(&remote_receipt)
        {
            return Err(SavedPlanError::StaleEvidence);
        }

        Ok(())
    }

    fn validate(&self) -> Result<(), SavedPlanError> {
        if self.format_version != FORMAT_VERSION
            || ConfigDigest::parse(self.config_digest.clone()).is_err()
            || !valid_hex_digest(&self.plan_digest)
            || !valid_hex_digest(&self.remote_receipt)
            || self.lineage != plan_uuid(&self.plan, "lineage")?
            || self.state_serial != plan_u64(&self.plan, "stateSerial")?
            || self.config_digest != plan_string(&self.plan, "configDigest")?
        {
            return Err(SavedPlanError::InvalidDocument);
        }
        let canonical =
            serde_json::to_vec(&self.plan).map_err(|_| SavedPlanError::InvalidDocument)?;
        if self.plan_digest != encode_hex(Sha256::digest(&canonical).as_slice()) {
            return Err(SavedPlanError::InvalidDocument);
        }

        Ok(())
    }
}

/// Reads and validates one bounded regular saved-plan file.
pub fn read(path: &Path) -> Result<SavedPlan, SavedPlanError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| SavedPlanError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_SAVED_PLAN_BYTES {
        return Err(SavedPlanError::UnsafeFile);
    }
    let file = File::open(path).map_err(|source| SavedPlanError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len()).map_err(|_| SavedPlanError::UnsafeFile)?,
    );
    file.take(MAX_SAVED_PLAN_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| SavedPlanError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() as u64 > MAX_SAVED_PLAN_BYTES || reject_duplicate_keys(&bytes).is_err() {
        return Err(SavedPlanError::InvalidDocument);
    }
    let document: SavedPlan =
        serde_json::from_slice(&bytes).map_err(|_| SavedPlanError::InvalidDocument)?;
    document.validate()?;

    Ok(document)
}

/// Writes one owner-only saved plan without replacing an existing path.
pub fn write_new(path: &Path, document: &SavedPlan) -> Result<(), SavedPlanError> {
    document.validate()?;
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = fs::canonicalize(parent).map_err(|source| SavedPlanError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    let file_name = path.file_name().ok_or(SavedPlanError::UnsafeFile)?;
    let target = parent.join(file_name);
    match fs::symlink_metadata(&target) {
        Ok(_) => return Err(SavedPlanError::AlreadyExists),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(SavedPlanError::Write {
                path: target,
                source,
            });
        }
    }
    let bytes = serde_json::to_vec_pretty(document).map_err(|_| SavedPlanError::InvalidDocument)?;
    if bytes.len() as u64 > MAX_SAVED_PLAN_BYTES {
        return Err(SavedPlanError::InvalidDocument);
    }
    let mut temporary =
        tempfile::NamedTempFile::new_in(&parent).map_err(|source| SavedPlanError::Write {
            path: target.clone(),
            source,
        })?;
    #[cfg(unix)]
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|source| SavedPlanError::Write {
            path: target.clone(),
            source,
        })?;
    temporary
        .write_all(&bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|source| SavedPlanError::Write {
            path: target.clone(),
            source,
        })?;
    temporary
        .persist_noclobber(&target)
        .map_err(|error| SavedPlanError::Write {
            path: target.clone(),
            source: error.error,
        })?;
    File::open(&parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| SavedPlanError::Write {
            path: target,
            source,
        })?;

    Ok(())
}

fn plan_string<'a>(plan: &'a serde_json::Value, field: &str) -> Result<&'a str, SavedPlanError> {
    plan.as_object()
        .and_then(|plan| plan.get(field))
        .and_then(serde_json::Value::as_str)
        .ok_or(SavedPlanError::InvalidDocument)
}

fn plan_uuid(plan: &serde_json::Value, field: &str) -> Result<Uuid, SavedPlanError> {
    plan_string(plan, field)?
        .parse()
        .map_err(|_| SavedPlanError::InvalidDocument)
}

fn plan_u64(plan: &serde_json::Value, field: &str) -> Result<u64, SavedPlanError> {
    plan.as_object()
        .and_then(|plan| plan.get(field))
        .and_then(serde_json::Value::as_u64)
        .ok_or(SavedPlanError::InvalidDocument)
}

fn valid_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
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

/// A saved plan that cannot be trusted or persisted safely.
#[derive(Debug, Error)]
pub enum SavedPlanError {
    #[error("only complete, applyable plans can be saved")]
    PlanBlocked,
    #[error("saved plan is malformed, tampered, or unsupported")]
    InvalidDocument,
    #[error("saved plan does not match fresh configuration, state, or Dokploy observations")]
    StaleEvidence,
    #[error("saved plan path is not a bounded regular file")]
    UnsafeFile,
    #[error("saved plan path already exists")]
    AlreadyExists,
    #[error("failed to read saved plan at {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to write saved plan at {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}
