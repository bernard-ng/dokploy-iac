//! Runtime Dokploy version policy (ADR 0014).

use std::io::Write;

use dokploy_sdk::{OperationRequest, Transport};
use dokploy_spec::{VersionCompatibility, embedded_versions};
use miette::{IntoDiagnostic, Result};

/// Whether a declarative command may mutate Dokploy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Access {
    Read,
    Mutate,
}

/// Runtime compatibility controls for one declarative command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Policy {
    pub access: Access,
    pub allow_unsupported: bool,
}

impl Policy {
    #[must_use]
    pub const fn new(access: Access, allow_unsupported: bool) -> Self {
        Self {
            access,
            allow_unsupported,
        }
    }
}

/// A successful server-version probe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerVersion {
    pub raw: String,
    pub compatibility: VersionCompatibility,
}

/// Reads and classifies the running Dokploy version.
pub async fn probe(transport: &impl Transport) -> Result<ServerVersion> {
    let response = transport
        .call(OperationRequest::new("settings.getDokployVersion"))
        .await
        .into_diagnostic()?;
    let raw = response
        .as_str()
        .ok_or_else(|| miette::miette!("Dokploy returned a non-text version"))?
        .to_owned();
    let versions = embedded_versions().map_err(|error| miette::miette!(error))?;
    let compatibility = versions
        .compatibility(&raw)
        .map_err(|_| miette::miette!("Dokploy returned an unreadable version `{raw}`"))?;

    Ok(ServerVersion { raw, compatibility })
}

/// Applies the runtime policy and writes any compatibility warning.
pub async fn enforce(
    transport: &impl Transport,
    access: Access,
    allow_unsupported: bool,
    diagnostics: &mut dyn Write,
) -> Result<Option<String>> {
    match probe(transport).await {
        Ok(server) => {
            match server.compatibility {
                VersionCompatibility::Supported => {}
                VersionCompatibility::NewerPatch => writeln!(
                    diagnostics,
                    "Warning: Dokploy {} is newer than the tested patch releases; proceeding within the supported minor line.",
                    server.raw
                )
                .into_diagnostic()?,
                VersionCompatibility::Unsupported
                    if access == Access::Mutate && !allow_unsupported =>
                {
                    return Err(miette::miette!(
                        "Dokploy {} is outside the supported versions; rerun with --allow-unsupported-version to permit this mutation",
                        server.raw
                    ));
                }
                VersionCompatibility::Unsupported => writeln!(
                    diagnostics,
                    "Warning: Dokploy {} is outside the supported versions; proceeding with an explicit or read-only command.",
                    server.raw
                )
                .into_diagnostic()?,
            }

            Ok(Some(server.raw))
        }
        Err(error) if access == Access::Mutate && !allow_unsupported => Err(miette::miette!(
            "cannot verify the Dokploy version before mutation: {error}; rerun with --allow-unsupported-version to permit this mutation"
        )),
        Err(error) => {
            writeln!(
                diagnostics,
                "Warning: cannot verify the Dokploy version: {error}; proceeding with an explicit or read-only command."
            )
            .into_diagnostic()?;
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dokploy_sim::Sim;

    use super::*;

    #[tokio::test]
    async fn simulator_reports_a_supported_version() {
        let specs = Arc::new(dokploy_spec::embedded().unwrap());
        let sim = Sim::new(specs);

        let version = probe(&sim).await.unwrap();

        assert_eq!(version.raw, "v0.30.6");
        assert_eq!(version.compatibility, VersionCompatibility::Supported);
    }

    #[tokio::test]
    async fn unsupported_mutations_need_the_explicit_override() {
        let specs = Arc::new(dokploy_spec::embedded().unwrap());
        let sim = Sim::new(specs);
        sim.tamper("settings.getDokployVersion", |response| {
            *response = serde_json::Value::String("v0.31.0".to_owned());
        });
        let mut diagnostics = Vec::new();

        let error = enforce(&sim, Access::Mutate, false, &mut diagnostics)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("--allow-unsupported-version"));

        enforce(&sim, Access::Read, false, &mut diagnostics)
            .await
            .unwrap();
        enforce(&sim, Access::Mutate, true, &mut diagnostics)
            .await
            .unwrap();
        let diagnostics = String::from_utf8(diagnostics).unwrap();
        assert!(diagnostics.contains("outside the supported versions"));
    }
}
