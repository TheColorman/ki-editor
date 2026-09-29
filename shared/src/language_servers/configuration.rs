use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde_json::Value;

use super::definition::ServerBehavior;
use crate::{absolute_path::AbsolutePath, language::LspServerConfig};

/// Resolves one server's settings in one workspace. Policy comes from the
/// server definition; token expansion and user-setting precedence are generic.
pub struct ServerConfiguration<'a> {
    config: &'a LspServerConfig,
    root: &'a AbsolutePath,
    behavior: &'static dyn ServerBehavior,
    placeholders: HashMap<&'static str, Option<String>>,
}

impl<'a> ServerConfiguration<'a> {
    pub fn new(config: &'a LspServerConfig, root: &'a AbsolutePath) -> Self {
        let behavior = config.behavior();
        Self::with_behavior(config, root, behavior)
    }

    fn with_behavior(
        config: &'a LspServerConfig,
        root: &'a AbsolutePath,
        behavior: &'static dyn ServerBehavior,
    ) -> Self {
        let placeholders = behavior
            .placeholders(root)
            .into_iter()
            .map(|placeholder| (placeholder.name, placeholder.value))
            .chain([
                (
                    "workspace",
                    Some(root.as_ref().to_string_lossy().into_owned()),
                ),
                ("lsp_data_dir", None),
            ])
            .collect();
        Self {
            config,
            root,
            behavior,
            placeholders,
        }
    }

    pub fn with_data_dir(mut self, directory: Option<&Path>) -> Self {
        self.placeholders.insert(
            "lsp_data_dir",
            directory.map(|path| path.to_string_lossy().into_owned()),
        );
        self
    }

    pub fn initialization_options(&self) -> Option<Value> {
        self.config
            .initialization_options()
            .map(|value| self.resolve_value(value))
    }

    pub fn configuration(&self, section: Option<&str>) -> Value {
        let configured = self
            .config
            .initialization_options()
            .and_then(|options| options.get("settings").cloned())
            .and_then(
                |settings| match section.filter(|section| !section.is_empty()) {
                    None => Some(settings),
                    Some(section) => section
                        .split('.')
                        .try_fold(&settings, |value, key| value.get(key))
                        .cloned(),
                },
            );
        self.resolve_value(
            configured
                .or_else(|| self.behavior.default_configuration(section))
                .unwrap_or(Value::Null),
        )
    }

    pub fn resolve_value(&self, value: Value) -> Value {
        match value {
            Value::String(value) => Value::String(self.resolve_string(&value)),
            Value::Array(values) => Value::Array(
                values
                    .into_iter()
                    .map(|value| self.resolve_value(value))
                    .collect(),
            ),
            Value::Object(values) => Value::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, self.resolve_value(value)))
                    .collect(),
            ),
            value => value,
        }
    }

    pub fn resolve_string(&self, value: &str) -> String {
        self.expand(value).0
    }

    pub fn resolve_path(&self, value: &str) -> Option<PathBuf> {
        let (value, resolved) = self.expand(value);
        if !resolved {
            return None;
        }
        let path = PathBuf::from(value);
        Some(if path.is_absolute() {
            path
        } else {
            self.root.as_ref().join(path)
        })
    }

    pub fn validation_warnings(&self) -> Vec<String> {
        self.config.initialization_options().into_iter()
            .flat_map(|options| self.behavior.configured_paths(&options))
            .filter_map(|path| match self.resolve_path(&path.value) {
                None => Some(format!("Unable to resolve configured {} path {:?}", path.label, path.value)),
                Some(resolved) if !resolved.exists() => Some(format!(
                    "Configured {} path {:?} does not exist (resolved to {}). Override this path in Ki config if the server uses a different installation.",
                    path.label, path.value, resolved.display(),
                )),
                Some(_) => None,
            }).collect()
    }

    /// Expand tokens in the original input once. Replacement values may contain
    /// token-like text (for example in a workspace name) and are never reprocessed.
    fn expand(&self, value: &str) -> (String, bool) {
        let mut output = String::with_capacity(value.len());
        let mut remaining = value;
        let mut resolved = true;
        while let Some(start) = remaining.find("${") {
            let Some(end) = remaining[start + 2..].find('}').map(|end| start + 2 + end) else {
                break;
            };
            output.push_str(&remaining[..start]);
            let name = &remaining[start + 2..end];
            if let Some(Some(replacement)) = self.placeholders.get(name) {
                output.push_str(replacement);
            } else {
                output.push_str(&remaining[start..=end]);
                resolved = false;
            }
            remaining = &remaining[end + 1..];
        }
        output.push_str(remaining);
        (output, resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::Command;
    use crate::language_servers::definition::{ConfiguredPath, Placeholder};

    struct TestServer;

    impl ServerBehavior for TestServer {
        fn default_configuration(&self, section: Option<&str>) -> Option<Value> {
            (section == Some("service.enabled")).then_some(Value::Bool(true))
        }

        fn placeholders(&self, root: &AbsolutePath) -> Vec<Placeholder> {
            vec![
                Placeholder {
                    name: "tool",
                    value: Some(root.as_ref().join("tool").display().to_string()),
                },
                Placeholder {
                    name: "missing",
                    value: None,
                },
            ]
        }

        fn configured_paths(&self, options: &Value) -> Vec<ConfiguredPath> {
            options
                .get("path")
                .and_then(Value::as_str)
                .map(|path| ConfiguredPath {
                    label: "test tool".to_string(),
                    value: path.to_string(),
                })
                .into_iter()
                .collect()
        }
    }

    #[test]
    fn server_placeholders_work_in_nested_values_and_paths() {
        let root: AbsolutePath = "/tmp/workspace".try_into().unwrap();
        let server = LspServerConfig::new("test", Command::new("test-server", &[]));
        let configuration = ServerConfiguration::with_behavior(&server, &root, &TestServer);
        assert_eq!(
            configuration.resolve_value(serde_json::json!({
                "paths": ["${workspace}/sdk", "${tool}/lib", "${missing}", "${unknown}"],
                "enabled": false
            })),
            serde_json::json!({
                "paths": ["/tmp/workspace/sdk", "/tmp/workspace/tool/lib", "${missing}", "${unknown}"],
                "enabled": false
            })
        );
        assert_eq!(
            configuration.resolve_path("${tool}/lib"),
            Some(PathBuf::from("/tmp/workspace/tool/lib"))
        );
        assert!(configuration.resolve_path("${missing}/lib").is_none());
        assert!(configuration.resolve_path("${unknown}").is_none());
    }

    #[test]
    fn validation_uses_only_paths_declared_by_the_server() {
        let root: AbsolutePath = "/tmp/workspace".try_into().unwrap();
        let server: LspServerConfig = serde_json::from_value(serde_json::json!({
            "id": "test", "command": { "command": "test-server", "arguments": [] },
            "initialization_options": { "path": "${missing}", "unrelated_path": "${other}" }
        }))
        .unwrap();
        let configuration = ServerConfiguration::with_behavior(&server, &root, &TestServer);
        let warnings = configuration.validation_warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("test tool"));
        assert!(warnings[0].contains("${missing}"));
        assert!(ServerConfiguration::new(&server, &root)
            .validation_warnings()
            .is_empty());
    }

    #[test]
    fn explicit_false_and_null_settings_override_server_defaults() {
        let root: AbsolutePath = "/tmp/workspace".try_into().unwrap();
        for enabled in [Value::Bool(false), Value::Null] {
            let server: LspServerConfig = serde_json::from_value(serde_json::json!({
                "id": "test", "command": { "command": "test-server", "arguments": [] },
                "initialization_options": { "settings": { "service": { "enabled": enabled } } }
            }))
            .unwrap();
            let configuration = ServerConfiguration::with_behavior(&server, &root, &TestServer);
            assert_eq!(
                configuration.configuration(Some("service.enabled")),
                enabled
            );
            assert_eq!(configuration.configuration(Some("unknown")), Value::Null);
        }
        let server = LspServerConfig::new("test", Command::new("test-server", &[]));
        assert_eq!(
            ServerConfiguration::with_behavior(&server, &root, &TestServer)
                .configuration(Some("service.enabled")),
            true
        );
    }
}
