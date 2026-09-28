use miette::Diagnostic;
use thiserror::Error;
use url::Url;

use crate::config::{ConfigError, ContextConfiguration};
use crate::credentials::{ApiKey, CredentialStore, CredentialStoreError};

pub const DOKPLOY_URL: &str = "DOKPLOY_URL";
pub const DOKPLOY_API_KEY: &str = "DOKPLOY_API_KEY";

pub struct ConnectionOptions {
    pub url: Option<String>,
    pub api_key: Option<ApiKey>,
}

impl std::fmt::Debug for ConnectionOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectionOptions")
            .field("url", &self.url.as_ref().map(|_| "[configured]"))
            .field("api_key", &self.api_key)
            .finish()
    }
}

pub struct ConnectionSettings {
    url: Url,
    api_key: ApiKey,
}

impl ConnectionSettings {
    #[must_use]
    pub fn url(&self) -> &Url {
        &self.url
    }

    #[must_use]
    pub fn api_key(&self) -> &ApiKey {
        &self.api_key
    }
}

impl std::fmt::Debug for ConnectionSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectionSettings")
            .field("url", &self.url)
            .field("api_key", &self.api_key)
            .finish()
    }
}

pub trait Environment {
    fn variable(&self, name: &str) -> Option<String>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessEnvironment;

impl Environment for ProcessEnvironment {
    fn variable(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

pub fn resolve_connection(
    options: ConnectionOptions,
    environment: &dyn Environment,
    configuration: &ContextConfiguration,
    credentials: &dyn CredentialStore,
) -> Result<ConnectionSettings, SettingsError> {
    let selected_context = configuration.current_context();
    let context = match selected_context {
        Some(name) => Some(configuration.context(Some(name))?.1),
        None => None,
    };
    let url = first_non_empty([
        options.url,
        environment.variable(DOKPLOY_URL),
        context.map(|context| context.url().to_owned()),
    ])
    .ok_or(SettingsError::MissingUrl)?;
    let cli_api_key = options.api_key.filter(|api_key| !api_key.is_empty());
    let environment_api_key = environment
        .variable(DOKPLOY_API_KEY)
        .filter(|value| !value.trim().is_empty())
        .map(ApiKey::new);
    let context_api_key = match selected_context {
        Some(name) if cli_api_key.is_none() && environment_api_key.is_none() => {
            credentials.get(name)?
        }
        _ => None,
    };
    let api_key = cli_api_key
        .or(environment_api_key)
        .or(context_api_key)
        .ok_or(SettingsError::MissingApiKey)?;

    Ok(ConnectionSettings {
        url: parse_base_url(&url)?,
        api_key,
    })
}

fn first_non_empty<const N: usize>(values: [Option<String>; N]) -> Option<String> {
    values
        .into_iter()
        .flatten()
        .find(|value| !value.trim().is_empty())
}

fn parse_base_url(value: &str) -> Result<Url, SettingsError> {
    let mut url = Url::parse(value).map_err(|source| SettingsError::InvalidUrl { source })?;

    if !matches!(url.scheme(), "http" | "https")
        || !url.has_host()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(SettingsError::UnsafeUrl);
    }

    if !url.path().ends_with('/') {
        let normalized_path = format!("{}/", url.path());
        url.set_path(&normalized_path);
    }

    Ok(url)
}

#[derive(Debug, Diagnostic, Error)]
pub enum SettingsError {
    #[error("no Dokploy URL is configured")]
    #[diagnostic(help("pass --url, set DOKPLOY_URL, or select a configured context"))]
    MissingUrl,

    #[error("no Dokploy API key is configured")]
    #[diagnostic(help(
        "pass --api-key, set DOKPLOY_API_KEY, or store a key for the selected context"
    ))]
    MissingApiKey,

    #[error("invalid Dokploy URL")]
    InvalidUrl {
        #[source]
        source: url::ParseError,
    },

    #[error("Dokploy URL must be an HTTP(S) origin without credentials, a query, or a fragment")]
    UnsafeUrl,

    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    CredentialStore(#[from] CredentialStoreError),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use super::{ConnectionOptions, DOKPLOY_API_KEY, DOKPLOY_URL, Environment, resolve_connection};
    use crate::config::ConfigRepository;
    use crate::credentials::{ApiKey, CredentialStore, CredentialStoreError};

    #[derive(Default)]
    struct MapEnvironment(BTreeMap<String, String>);

    impl Environment for MapEnvironment {
        fn variable(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
    }

    struct MemoryCredentialStore {
        values: BTreeMap<String, ApiKey>,
    }

    impl CredentialStore for MemoryCredentialStore {
        fn get(&self, context: &str) -> Result<Option<ApiKey>, CredentialStoreError> {
            Ok(self.values.get(context).cloned())
        }

        fn set(&self, _context: &str, _api_key: &ApiKey) -> Result<(), CredentialStoreError> {
            Ok(())
        }

        fn delete(&self, _context: &str) -> Result<(), CredentialStoreError> {
            Ok(())
        }
    }

    fn configuration() -> crate::config::ContextConfiguration {
        let temporary_directory = tempfile::tempdir().expect("temporary directory is available");
        let path = temporary_directory.path().join("config.toml");
        fs::write(
            &path,
            r#"current_context = "production"

[contexts.production]
url = "https://context.example.com"
"#,
        )
        .expect("configuration fixture is writable");

        ConfigRepository::new(path)
            .load()
            .expect("fixture configuration is valid")
    }

    #[test]
    fn command_line_values_have_highest_precedence() {
        let configuration = configuration();
        let environment = MapEnvironment(BTreeMap::from([
            (DOKPLOY_URL.to_owned(), "https://env.example.com".to_owned()),
            (DOKPLOY_API_KEY.to_owned(), "environment-key".to_owned()),
        ]));
        let credentials = MemoryCredentialStore {
            values: BTreeMap::from([("production".to_owned(), ApiKey::new("context-key"))]),
        };

        let settings = resolve_connection(
            ConnectionOptions {
                url: Some("https://cli.example.com".to_owned()),
                api_key: Some(ApiKey::new("cli-key")),
            },
            &environment,
            &configuration,
            &credentials,
        )
        .expect("settings resolve");

        assert_eq!(settings.url().as_str(), "https://cli.example.com/");
        assert_eq!(settings.api_key().expose(), "cli-key");
    }

    #[test]
    fn environment_values_override_the_selected_context() {
        let configuration = configuration();
        let environment = MapEnvironment(BTreeMap::from([
            (DOKPLOY_URL.to_owned(), "https://env.example.com".to_owned()),
            (DOKPLOY_API_KEY.to_owned(), "environment-key".to_owned()),
        ]));
        let credentials = MemoryCredentialStore {
            values: BTreeMap::from([("production".to_owned(), ApiKey::new("context-key"))]),
        };

        let settings = resolve_connection(
            ConnectionOptions {
                url: None,
                api_key: None,
            },
            &environment,
            &configuration,
            &credentials,
        )
        .expect("settings resolve");

        assert_eq!(settings.url().as_str(), "https://env.example.com/");
        assert_eq!(settings.api_key().expose(), "environment-key");
    }

    #[test]
    fn selected_context_is_the_final_fallback() {
        let configuration = configuration();
        let credentials = MemoryCredentialStore {
            values: BTreeMap::from([("production".to_owned(), ApiKey::new("context-key"))]),
        };

        let settings = resolve_connection(
            ConnectionOptions {
                url: None,
                api_key: None,
            },
            &MapEnvironment::default(),
            &configuration,
            &credentials,
        )
        .expect("settings resolve");

        assert_eq!(settings.url().as_str(), "https://context.example.com/");
        assert_eq!(settings.api_key().expose(), "context-key");
    }

    #[test]
    fn debug_output_redacts_api_keys() {
        let options = ConnectionOptions {
            url: Some("https://deploy.example.com".to_owned()),
            api_key: Some(ApiKey::new("do-not-print-this")),
        };

        let output = format!("{options:?}");

        assert!(!output.contains("do-not-print-this"));
        assert!(output.contains("[REDACTED]"));
    }

    #[test]
    fn diagnostics_do_not_repeat_a_url_that_contains_credentials() {
        let configuration = configuration();
        let credentials = MemoryCredentialStore {
            values: BTreeMap::new(),
        };

        let error = resolve_connection(
            ConnectionOptions {
                url: Some("https://admin:secret@deploy.example.com".to_owned()),
                api_key: Some(ApiKey::new("api-key")),
            },
            &MapEnvironment::default(),
            &configuration,
            &credentials,
        )
        .expect_err("credentials in URLs are rejected");
        let output = error.to_string();

        assert!(!output.contains("admin"));
        assert!(!output.contains("secret"));
    }
}
