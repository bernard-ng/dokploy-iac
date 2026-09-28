use keyring::{Entry, Error as KeyringError};
use miette::Diagnostic;
use thiserror::Error;
use zeroize::Zeroizing;

const KEYRING_SERVICE: &str = "dokploy";

#[derive(Clone)]
pub struct ApiKey(Zeroizing<String>);

impl ApiKey {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ApiKey([REDACTED])")
    }
}

pub trait CredentialStore {
    fn get(&self, context: &str) -> Result<Option<ApiKey>, CredentialStoreError>;

    fn set(&self, context: &str, api_key: &ApiKey) -> Result<(), CredentialStoreError>;

    fn delete(&self, context: &str) -> Result<(), CredentialStoreError>;
}

#[derive(Clone, Debug, Default)]
pub struct KeyringCredentialStore;

impl CredentialStore for KeyringCredentialStore {
    fn get(&self, context: &str) -> Result<Option<ApiKey>, CredentialStoreError> {
        let entry = entry(context)?;

        match entry.get_password() {
            Ok(value) => Ok(Some(ApiKey::new(value))),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(source) => Err(CredentialStoreError::Access {
                context: context.to_owned(),
                source,
            }),
        }
    }

    fn set(&self, context: &str, api_key: &ApiKey) -> Result<(), CredentialStoreError> {
        entry(context)?
            .set_password(api_key.expose())
            .map_err(|source| CredentialStoreError::Access {
                context: context.to_owned(),
                source,
            })
    }

    fn delete(&self, context: &str) -> Result<(), CredentialStoreError> {
        match entry(context)?.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(source) => Err(CredentialStoreError::Access {
                context: context.to_owned(),
                source,
            }),
        }
    }
}

fn entry(context: &str) -> Result<Entry, CredentialStoreError> {
    Entry::new(KEYRING_SERVICE, context).map_err(|source| CredentialStoreError::Access {
        context: context.to_owned(),
        source,
    })
}

#[derive(Debug, Diagnostic, Error)]
pub enum CredentialStoreError {
    #[error("could not access the API key for context `{context}`")]
    #[diagnostic(help("check that the operating system credential store is available"))]
    Access {
        context: String,
        #[source]
        source: KeyringError,
    },
}

#[cfg(test)]
mod tests {
    use super::ApiKey;

    #[test]
    fn debug_output_never_contains_the_secret() {
        let secret = "a-very-secret-api-key";
        let api_key = ApiKey::new(secret);

        let debug_output = format!("{api_key:?}");

        assert_eq!(debug_output, "ApiKey([REDACTED])");
        assert!(!debug_output.contains(secret));
    }
}
