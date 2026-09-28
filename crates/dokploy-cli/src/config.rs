use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use miette::Diagnostic;
use thiserror::Error;
use toml_edit::{DocumentMut, Item, value};
use url::Url;

const CONFIG_FILE_NAME: &str = "config.toml";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Context {
    url: String,
}

impl Context {
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }
}

#[derive(Clone)]
pub struct ContextConfiguration {
    current_context: Option<String>,
    contexts: BTreeMap<String, Context>,
    document: DocumentMut,
}

impl std::fmt::Debug for ContextConfiguration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ContextConfiguration")
            .field("current_context", &self.current_context)
            .field("context_names", &self.contexts.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl ContextConfiguration {
    #[must_use]
    pub fn current_context(&self) -> Option<&str> {
        self.current_context.as_deref()
    }

    #[must_use]
    pub fn contexts(&self) -> &BTreeMap<String, Context> {
        &self.contexts
    }

    pub fn context(&self, requested_name: Option<&str>) -> Result<(&str, &Context), ConfigError> {
        let name = match requested_name {
            Some(name) => name,
            None => self
                .current_context()
                .ok_or(ConfigError::NoCurrentContext)?,
        };
        let (configured_name, context) = self
            .contexts
            .get_key_value(name)
            .ok_or_else(|| ConfigError::UnknownContext(name.to_owned()))?;

        Ok((configured_name.as_str(), context))
    }
}

#[derive(Clone, Debug)]
pub struct ConfigRepository {
    path: PathBuf,
}

impl ConfigRepository {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn platform() -> Result<Self, ConfigError> {
        let directories = ProjectDirs::from("dev", "LegalterLaw", "dokploy")
            .ok_or(ConfigError::ConfigDirectoryUnavailable)?;

        Ok(Self::new(directories.config_dir().join(CONFIG_FILE_NAME)))
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<ContextConfiguration, ConfigError> {
        let source = match fs::read_to_string(&self.path) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(source) => {
                return Err(ConfigError::Read {
                    path: self.path.clone(),
                    source,
                });
            }
        };

        parse_configuration(&source, &self.path)
    }

    pub fn select(&self, name: &str) -> Result<(), ConfigError> {
        let mut configuration = self.load()?;

        if !configuration.contexts.contains_key(name) {
            return Err(ConfigError::UnknownContext(name.to_owned()));
        }

        configuration.document["current_context"] = value(name);
        self.write_atomically(configuration.document.to_string().as_bytes())
    }

    fn write_atomically(&self, contents: &[u8]) -> Result<(), ConfigError> {
        let parent = self.path.parent().ok_or_else(|| ConfigError::Write {
            path: self.path.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "configuration path has no parent directory",
            ),
        })?;

        fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
            path: self.path.clone(),
            source,
        })?;

        let mut temporary_file =
            tempfile::NamedTempFile::new_in(parent).map_err(|source| ConfigError::Write {
                path: self.path.clone(),
                source,
            })?;

        temporary_file
            .write_all(contents)
            .and_then(|()| temporary_file.as_file().sync_all())
            .map_err(|source| ConfigError::Write {
                path: self.path.clone(),
                source,
            })?;

        temporary_file
            .persist(&self.path)
            .map(|_| ())
            .map_err(|error| ConfigError::Write {
                path: self.path.clone(),
                source: error.error,
            })
    }
}

fn parse_configuration(source: &str, path: &Path) -> Result<ContextConfiguration, ConfigError> {
    let document = source
        .parse::<DocumentMut>()
        .map_err(|_| ConfigError::Parse {
            path: path.to_path_buf(),
        })?;
    let current_context = optional_string(&document, "current_context", path)?;
    let mut contexts = BTreeMap::new();

    if let Some(context_items) = document.get("contexts") {
        let context_table = context_items
            .as_table()
            .ok_or_else(|| ConfigError::Invalid {
                path: path.to_path_buf(),
                message: "`contexts` must be a table".to_owned(),
            })?;

        for (name, item) in context_table {
            let context = parse_context(name, item, path)?;
            contexts.insert(name.to_owned(), context);
        }
    }

    if let Some(name) = current_context.as_deref()
        && !contexts.contains_key(name)
    {
        return Err(ConfigError::Invalid {
            path: path.to_path_buf(),
            message: format!("current context `{name}` is not defined in `contexts`"),
        });
    }

    Ok(ContextConfiguration {
        current_context,
        contexts,
        document,
    })
}

fn optional_string(
    document: &DocumentMut,
    key: &str,
    path: &Path,
) -> Result<Option<String>, ConfigError> {
    match document.get(key) {
        Some(item) => item
            .as_str()
            .map(|value| Some(value.to_owned()))
            .ok_or_else(|| ConfigError::Invalid {
                path: path.to_path_buf(),
                message: format!("`{key}` must be a string"),
            }),
        None => Ok(None),
    }
}

fn parse_context(name: &str, item: &Item, path: &Path) -> Result<Context, ConfigError> {
    let table = item.as_table().ok_or_else(|| ConfigError::Invalid {
        path: path.to_path_buf(),
        message: format!("context `{name}` must be a table"),
    })?;
    let url = table
        .get("url")
        .and_then(Item::as_str)
        .filter(|url| !url.trim().is_empty())
        .ok_or_else(|| ConfigError::Invalid {
            path: path.to_path_buf(),
            message: format!("context `{name}` must define a non-empty string `url`"),
        })?;

    if !is_safe_http_url(url) {
        return Err(ConfigError::Invalid {
            path: path.to_path_buf(),
            message: format!(
                "context `{name}` must define an HTTP(S) URL without credentials, a query, or a fragment"
            ),
        });
    }

    Ok(Context {
        url: url.to_owned(),
    })
}

fn is_safe_http_url(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };

    matches!(url.scheme(), "http" | "https")
        && url.has_host()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

#[derive(Debug, Diagnostic, Error)]
pub enum ConfigError {
    #[error("the platform configuration directory is unavailable")]
    #[diagnostic(help("set DOKPLOY_URL and DOKPLOY_API_KEY or use a supported home directory"))]
    ConfigDirectoryUnavailable,

    #[error("failed to read configuration at {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse configuration at {path}")]
    #[diagnostic(help("fix the TOML syntax in the configuration file"))]
    Parse { path: PathBuf },

    #[error("invalid configuration at {path}: {message}")]
    Invalid { path: PathBuf, message: String },

    #[error("failed to write configuration at {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("no current context is selected")]
    #[diagnostic(help("select one with `dokploy context use <name>`"))]
    NoCurrentContext,

    #[error("unknown context `{0}`")]
    #[diagnostic(help("list configured contexts with `dokploy context list`"))]
    UnknownContext(String),
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{ConfigError, ConfigRepository};

    #[test]
    fn missing_file_loads_as_an_empty_configuration() {
        let temporary_directory = tempfile::tempdir().expect("temporary directory is available");
        let repository = ConfigRepository::new(temporary_directory.path().join("config.toml"));

        let configuration = repository.load().expect("missing file is allowed");

        assert_eq!(configuration.current_context(), None);
        assert!(configuration.contexts().is_empty());
    }

    #[test]
    fn selecting_a_context_preserves_comments_and_other_contexts() {
        let temporary_directory = tempfile::tempdir().expect("temporary directory is available");
        let path = temporary_directory
            .path()
            .join("nested")
            .join("config.toml");
        fs::create_dir_all(path.parent().expect("path has a parent"))
            .expect("fixture directory is writable");
        fs::write(
            &path,
            r#"# Keep this comment.
current_context = "staging"

[contexts.production]
url = "https://deploy.example.com"

[contexts.staging]
url = "https://staging.example.com"
"#,
        )
        .expect("configuration fixture is writable");
        let repository = ConfigRepository::new(path.clone());

        repository
            .select("production")
            .expect("known context can be selected");

        let source = fs::read_to_string(path).expect("updated configuration is readable");
        assert!(source.contains("# Keep this comment."));
        assert!(source.contains("current_context = \"production\""));
        assert!(source.contains("[contexts.staging]"));
    }

    #[test]
    fn selecting_an_unknown_context_does_not_change_the_file() {
        let temporary_directory = tempfile::tempdir().expect("temporary directory is available");
        let path = temporary_directory.path().join("config.toml");
        let original = r#"[contexts.production]
url = "https://deploy.example.com"
"#;
        fs::write(&path, original).expect("configuration fixture is writable");
        let repository = ConfigRepository::new(path.clone());

        let error = repository
            .select("missing")
            .expect_err("unknown context is rejected");

        assert!(matches!(error, ConfigError::UnknownContext(name) if name == "missing"));
        assert_eq!(
            fs::read_to_string(path).expect("configuration is readable"),
            original
        );
    }

    #[test]
    fn context_urls_cannot_embed_credentials() {
        let temporary_directory = tempfile::tempdir().expect("temporary directory is available");
        let path = temporary_directory.path().join("config.toml");
        fs::write(
            &path,
            r#"[contexts.production]
url = "https://admin:secret@deploy.example.com"
"#,
        )
        .expect("configuration fixture is writable");

        let error = ConfigRepository::new(path)
            .load()
            .expect_err("credentials in URLs are rejected");

        assert!(
            matches!(error, ConfigError::Invalid { message, .. } if !message.contains("secret"))
        );
    }

    #[test]
    fn configuration_debug_output_excludes_unrecognized_values() {
        let temporary_directory = tempfile::tempdir().expect("temporary directory is available");
        let path = temporary_directory.path().join("config.toml");
        fs::write(
            &path,
            r#"custom_value = "must-not-appear"

[contexts.production]
url = "https://deploy.example.com"
"#,
        )
        .expect("configuration fixture is writable");

        let configuration = ConfigRepository::new(path)
            .load()
            .expect("configuration is valid");
        let output = format!("{configuration:?}");

        assert!(!output.contains("must-not-appear"));
        assert!(output.contains("production"));
    }
}
