use crate::language::{Command, LspCommand};
use itertools::Itertools;
use serde::Deserialize;
use serde_json::Value;

use super::{
    definition::{ServerBehavior, ServerDefinition},
    package_workspace,
    workspace_root::{RootFallback, RootMarker, WorkspaceRootPolicy},
};

pub const DEFINITION: ServerDefinition = ServerDefinition {
    ids: &["omnisharp"],
    executables: &["omnisharp", "OmniSharp"],
    behavior: &OmniSharp,
    root_policy: &ROOT_POLICY,
};

const ROOT_POLICY: WorkspaceRootPolicy = WorkspaceRootPolicy {
    marker_groups: &[
        &[RootMarker::Extensions(&["sln", "slnx"])],
        &[RootMarker::Extensions(&["csproj"])],
        // These markers share a priority: the nearest package of either kind wins.
        &[
            package_workspace::WORKSPACE_MARKERS,
            package_workspace::PROJECT_MARKERS,
        ],
    ],
    fallback: RootFallback::Boundary,
    canonicalize: false,
};

pub fn command() -> LspCommand {
    LspCommand {
        command: Command::new("omnisharp", &["--languageserver"]),
        // A design-time build should retain its project model even when cached
        // restore diagnostics contain errors. Normal builds are unaffected.
        environment: [("ContinueOnError".into(), "WarnAndContinue".into())].into(),
        ..LspCommand::default()
    }
}

struct OmniSharp;

impl ServerBehavior for OmniSharp {
    fn notification_warning(&self, method: &str, params: &Value) -> Option<String> {
        if method != "o#/msbuildprojectdiagnostics" {
            return None;
        }
        let diagnostics: ProjectDiagnostics = serde_json::from_value(params.clone()).ok()?;
        let count = diagnostics.errors.len() + diagnostics.warnings.len();
        if count == 0 {
            return None;
        }
        let details = diagnostics
            .errors
            .iter()
            .chain(&diagnostics.warnings)
            .take(5)
            .map(|diagnostic| diagnostic.text.as_str())
            .join("\n\n");
        let omitted = if count > 5 {
            format!("\n\n{} more diagnostics; see `ki @ log lsp`.", count - 5)
        } else {
            String::new()
        };
        Some(format!(
            "OmniSharp reported problems while loading {}.\nLanguage features may be incomplete.\n\n{details}{omitted}",
            diagnostics.file_name
        ))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ProjectDiagnostics {
    file_name: String,
    #[serde(default)]
    errors: Vec<ProjectDiagnostic>,
    #[serde(default)]
    warnings: Vec<ProjectDiagnostic>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ProjectDiagnostic {
    text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn design_time_builds_continue_without_disabling_audit() {
        let environment = command().environment;
        assert_eq!(environment["ContinueOnError"], "WarnAndContinue");
        assert!(!environment.contains_key("NuGetAudit"));
        assert!(!environment.contains_key("EmitAssetsLogMessages"));
    }

    #[test]
    fn csharp_inherits_the_environment_and_allows_strict_loading() {
        let language = crate::languages::languages().remove("c_sharp").unwrap();
        let servers = language.lsp_server_configs();
        assert_eq!(
            servers[0].process_command().environment()["ContinueOnError"],
            "WarnAndContinue"
        );
        let mut configured = serde_json::to_value(language).unwrap();
        configured["lsp_command"]["environment"]["ContinueOnError"] = Value::String("false".into());
        let configured: crate::language::Language = serde_json::from_value(configured).unwrap();
        assert_eq!(
            configured.lsp_server_configs()[0]
                .process_command()
                .environment()["ContinueOnError"],
            "false"
        );
    }

    #[test]
    fn project_diagnostics_expose_errors_and_warnings() {
        let warning = OmniSharp
            .notification_warning(
                "o#/msbuildprojectdiagnostics",
                &serde_json::json!({
                    "FileName": "/project/App.csproj",
                    "Errors": [{ "Text": "Warning As Error: Package has a known vulnerability" }],
                    "Warnings": [{ "Text": "An unresolved reference" }]
                }),
            )
            .unwrap();
        assert!(warning.contains("/project/App.csproj"));
        assert!(warning.contains("Language features may be incomplete"));
        assert!(warning.contains("Package has a known vulnerability"));
        assert!(warning.contains("An unresolved reference"));
    }

    #[test]
    fn clean_malformed_and_unrelated_notifications_are_quiet() {
        for (method, params) in [
            (
                "o#/msbuildprojectdiagnostics",
                serde_json::json!({ "FileName": "/project/App.csproj", "Errors": [], "Warnings": [] }),
            ),
            (
                "o#/msbuildprojectdiagnostics",
                serde_json::json!({ "Errors": [{}] }),
            ),
            (
                "o#/projectchanged",
                serde_json::json!({ "FileName": "/project/App.csproj", "Errors": [{ "Text": "not a diagnostic notification" }] }),
            ),
        ] {
            assert!(OmniSharp.notification_warning(method, &params).is_none());
        }
    }

    #[test]
    fn project_warning_limits_details_and_points_to_the_log() {
        let warning = OmniSharp
            .notification_warning(
                "o#/msbuildprojectdiagnostics",
                &serde_json::json!({
                    "FileName": "/project/App.csproj",
                    "Errors": (0..10).map(|index| serde_json::json!({ "Text": format!("problem {index}") })).collect::<Vec<_>>()
                }),
            )
            .unwrap();
        assert!(warning.contains("problem 4"));
        assert!(!warning.contains("problem 5"));
        assert!(warning.contains("5 more diagnostics; see `ki @ log lsp`"));
    }
}
