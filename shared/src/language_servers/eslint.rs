use serde_json::{json, Value};

use crate::language::{Command, LanguageId, LspDiagnosticMode, LspServerConfig};

use super::definition::{ServerBehavior, ServerDefinition};

pub const DEFINITION: ServerDefinition = ServerDefinition {
    ids: &["eslint"],
    executables: &["vscode-eslint-language-server"],
    behavior: &Eslint,
    root_policy: &super::package_workspace::ROOT_POLICY,
};

struct Eslint;

impl ServerBehavior for Eslint {
    fn default_configuration(&self, section: Option<&str>) -> Option<Value> {
        default_configuration(section)
    }

    fn error_hint(&self, message: &str) -> Option<&'static str> {
        error_hint(message)
    }
}

pub fn config(language_id: &'static str) -> LspServerConfig {
    LspServerConfig {
        language_id: Some(LanguageId::new(language_id)),
        primary: false,
        diagnostic_mode: LspDiagnosticMode::Pull,
        ..LspServerConfig::new(
            "eslint",
            Command::new("vscode-eslint-language-server", &["--stdio"]),
        )
    }
}

fn default_configuration(section: Option<&str>) -> Option<Value> {
    let section = section?;
    let settings = json!({
        "enable": true,
        "run": "onType",
        "validate": ["javascript", "javascriptreact", "typescript", "typescriptreact", "vue"],
        "probe": ["javascript", "javascriptreact", "typescript", "typescriptreact", "vue"],
        "workingDirectories": [{ "mode": "auto" }]
    });
    if section == "eslint" {
        Some(settings)
    } else {
        settings.get(section.strip_prefix("eslint.")?).cloned()
    }
}

fn error_hint(message: &str) -> Option<&'static str> {
    message.contains("The \"path\" argument must be of type string").then_some(
        "The ESLint server failed while resolving a file path. Check that the ESLint server, eslint package, parser/plugins, and working-directory/config setup match your project. If using a non-node_modules install, ensure the server can still resolve the project eslint package and vue parser.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        absolute_path::AbsolutePath, language_servers::configuration::ServerConfiguration,
    };

    #[test]
    fn defaults_and_hints_are_scoped_to_eslint() {
        let root: AbsolutePath = "/tmp/project".try_into().unwrap();
        let server = config("vue");
        let configuration = ServerConfiguration::new(&server, &root);
        let full = configuration.configuration(Some("eslint"));
        for key in ["enable", "run", "validate", "probe", "workingDirectories"] {
            assert_eq!(
                configuration.configuration(Some(&format!("eslint.{key}"))),
                full[key]
            );
        }
        assert!(full["validate"].as_array().unwrap().contains(&json!("vue")));
        assert_eq!(configuration.configuration(Some("vtsls")), Value::Null);
        assert_eq!(configuration.configuration(Some("typescript")), Value::Null);
        assert_eq!(
            configuration.resolve_string("${vue_typescript_plugin}"),
            "${vue_typescript_plugin}"
        );
        assert!(server
            .behavior()
            .error_hint("The \"path\" argument must be of type string")
            .unwrap()
            .contains("ESLint"));
        assert!(server
            .behavior()
            .error_hint("Cannot find provider for definition")
            .is_none());
    }
}
