use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    path::Path,
};

use dokploy_core::{
    ChangeKind, OwnedValue, PropertyObservation, PropertyUnknownReason, RemoteObservation,
    RemoteResource, RemoteState, StoredState, plan,
};
use dokploy_state::{
    ExpectedState, InstanceIdentity, ManagedInputs, OperationJournal, PlanDigest, RemoteId,
    ResourceAddress, ResourceKind, ResourceState, SensitiveInputs, SensitivePropertyPath,
    StateFile, StateStore,
};
use semver::Version;
use zeroize::Zeroizing;

use crate::sensitive::{SensitiveFingerprintError, SensitiveFingerprinter};

use super::*;

struct ExistingFingerprinterLoader {
    id: &'static str,
    key: [u8; 32],
    loads: Cell<usize>,
}

impl SensitiveFingerprinterLoader for ExistingFingerprinterLoader {
    fn load(
        &self,
        instance: InstanceIdentity,
    ) -> Result<SensitiveFingerprinter, SensitiveFingerprintError> {
        self.loads.set(self.loads.get() + 1);
        Ok(SensitiveFingerprinter::fixture(instance, self.id, self.key))
    }
}

struct PanicFingerprinterLoader;

impl SensitiveFingerprinterLoader for PanicFingerprinterLoader {
    fn load(
        &self,
        _instance: InstanceIdentity,
    ) -> Result<SensitiveFingerprinter, SensitiveFingerprintError> {
        panic!("preflight must complete before credential access")
    }
}

struct FailingFingerprinterLoader;

impl SensitiveFingerprinterLoader for FailingFingerprinterLoader {
    fn load(
        &self,
        _instance: InstanceIdentity,
    ) -> Result<SensitiveFingerprinter, SensitiveFingerprintError> {
        Err(SensitiveFingerprintError::StoreUnavailable)
    }
}

struct PanicSourceResolver;

impl SensitiveSourceResolver for PanicSourceResolver {
    fn environment(&self, _name: &str) -> EnvironmentSource {
        panic!("source resolution must not occur")
    }

    fn file(
        &self,
        _workspace: &Path,
        _path: &str,
    ) -> Result<Zeroizing<Vec<u8>>, SensitiveFileReadError> {
        panic!("source resolution must not occur")
    }
}

#[derive(Default)]
struct RecordingSourceResolver {
    environment: BTreeMap<String, Result<Vec<u8>, ()>>,
    files: BTreeMap<String, Result<Vec<u8>, SensitiveFileReadError>>,
    calls: RefCell<Vec<String>>,
}

impl SensitiveSourceResolver for RecordingSourceResolver {
    fn environment(&self, name: &str) -> EnvironmentSource {
        self.calls.borrow_mut().push(format!("env:{name}"));
        match self.environment.get(name) {
            None => EnvironmentSource::Missing,
            Some(Err(())) => EnvironmentSource::NotUtf8,
            Some(Ok(value)) => EnvironmentSource::Value(Zeroizing::new(value.clone())),
        }
    }

    fn file(
        &self,
        _workspace: &Path,
        path: &str,
    ) -> Result<Zeroizing<Vec<u8>>, SensitiveFileReadError> {
        self.calls.borrow_mut().push(format!("file:{path}"));
        self.files
            .get(path)
            .cloned()
            .unwrap_or(Err(SensitiveFileReadError::Unavailable))
            .map(Zeroizing::new)
    }
}

fn existing_fingerprinter_loader() -> ExistingFingerprinterLoader {
    existing_fingerprinter_loader_with("0199a0c8-2351-7c31-8899-2c8f81983ea5", [7; 32])
}

fn existing_fingerprinter_loader_with(
    id: &'static str,
    key: [u8; 32],
) -> ExistingFingerprinterLoader {
    ExistingFingerprinterLoader {
        id,
        key,
        loads: Cell::new(0),
    }
}

fn instance() -> InstanceIdentity {
    InstanceIdentity::parse("https://deploy.example.test").unwrap()
}

fn digest(character: char) -> ConfigDigest {
    ConfigDigest::parse(character.to_string().repeat(64)).unwrap()
}

mod convergence;
mod file_security;
mod sources;
