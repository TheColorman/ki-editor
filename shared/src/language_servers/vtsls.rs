use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::{
    absolute_path::AbsolutePath,
    language::{Command, LanguageId, LspDiagnosticMode, LspServerConfig},
};

use super::definition::{ConfiguredPath, Placeholder, ServerBehavior, ServerDefinition};

pub const DEFINITION: ServerDefinition = ServerDefinition {
    ids: &["vue", "vtsls"],
    executables: &["vtsls"],
    behavior: &Vtsls,
    root_policy: &super::package_workspace::ROOT_POLICY,
};

struct Vtsls;

impl ServerBehavior for Vtsls {
    fn default_configuration(&self, section: Option<&str>) -> Option<Value> {
        default_configuration(section)
    }

    fn error_hint(&self, message: &str) -> Option<&'static str> {
        error_hint(message)
    }

    fn placeholders(&self, root: &AbsolutePath) -> Vec<Placeholder> {
        vec![Placeholder {
            name: "vue_typescript_plugin",
            value: vue_typescript_plugin_location(root).map(|path| path.display().to_string()),
        }]
    }

    fn configured_paths(&self, options: &Value) -> Vec<ConfiguredPath> {
        let plugins = options
            .pointer("/vtsls/tsserver/globalPlugins")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|plugin| {
                Some(ConfiguredPath {
                    label: format!(
                        "plugin {}",
                        plugin
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("<unknown plugin>")
                    ),
                    value: plugin.get("location")?.as_str()?.to_string(),
                })
            });
        super::typescript_language_server::sdk_paths(options)
            .into_iter()
            .chain(plugins)
            .collect()
    }
}

pub fn vue() -> LspServerConfig {
    LspServerConfig {
        language_id: Some(LanguageId::new("vue")),
        initialization_options: Some(json!({
            "typescript": {
                "tsdk": "${workspace}/node_modules/typescript/lib"
            },
            "vtsls": vue_settings()
        })),
        diagnostic_mode: LspDiagnosticMode::Both,
        ..LspServerConfig::new("vue", Command::new("vtsls", &["--stdio"]))
    }
}

fn vue_settings() -> Value {
    json!({
        "tsserver": {
            "globalPlugins": [{
                "name": "@vue/typescript-plugin",
                "location": "${vue_typescript_plugin}",
                "languages": ["vue"],
                "enableForWorkspaceTypeScriptVersions": true
            }]
        }
    })
}

fn default_configuration(section: Option<&str>) -> Option<Value> {
    match section {
        Some("") => Some(json!({
            "typescript.tsdk": "${workspace}/node_modules/typescript/lib",
            "typescript.validate.enable": true,
            "javascript.validate.enable": true,
            "vtsls": vue_settings()
        })),
        Some("typescript") => Some(json!({
            "tsdk": "${workspace}/node_modules/typescript/lib",
            "validate": { "enable": true }
        })),
        Some("vtsls") => Some(vue_settings()),
        _ => None,
    }
}

fn error_hint(message: &str) -> Option<&'static str> {
    message.contains("Cannot find provider for definition").then_some(
        "For .vue files with vtsls, ensure the Vue TypeScript plugin is installed and configured for the server you are running. If using Nix-managed vtsls, make sure @vue/typescript-plugin is also available to that vtsls instance, or override the Vue lsp_servers initialization_options in Ki config to point to the plugin location.",
    )
}

fn vue_typescript_plugin_location(root: &AbsolutePath) -> Option<PathBuf> {
    let workspace_plugin = root.as_ref().join("node_modules/@vue/typescript-plugin");
    if workspace_plugin.exists() {
        return Some(workspace_plugin);
    }

    let vue_language_server = find_in_path("vue-language-server")?;
    let vue_language_server = std::fs::canonicalize(vue_language_server).ok()?;

    find_package_ancestor(&vue_language_server, "@vue/language-server")
        .or_else(|| {
            vue_language_server
                .parent()
                .and_then(Path::parent)
                .map(|root| root.join("lib/language-tools/packages/language-server"))
                .filter(|path| path.exists())
        })
        .or_else(|| {
            vue_language_server
                .parent()
                .and_then(Path::parent)
                .map(|root| root.join("lib/node_modules/@vue/language-server"))
                .filter(|path| path.exists())
        })
}

fn find_package_ancestor(path: &Path, package_name: &str) -> Option<PathBuf> {
    path.ancestors().find_map(|ancestor| {
        let package_json = ancestor.join("package.json");
        let content = std::fs::read_to_string(package_json).ok()?;
        let value: Value = serde_json::from_str(&content).ok()?;
        (value.get("name").and_then(|name| name.as_str()) == Some(package_name))
            .then(|| ancestor.to_path_buf())
    })
}

fn find_in_path(command: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(':')
        .find_map(|dir| {
            let path = PathBuf::from(dir).join(command);
            path.exists().then_some(path)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language_servers::configuration::ServerConfiguration;

    #[test]
    fn resolves_and_validates_workspace_sdk_and_vue_plugin() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let root: AbsolutePath = temp.path().try_into()?;
        let plugin = temp.path().join("node_modules/@vue/typescript-plugin");
        std::fs::create_dir_all(&plugin)?;
        std::fs::create_dir_all(temp.path().join("node_modules/typescript/lib"))?;
        let server = vue();
        let configuration = ServerConfiguration::new(&server, &root);
        let options = configuration.initialization_options().unwrap();
        assert_eq!(
            options["vtsls"]["tsserver"]["globalPlugins"][0]["location"],
            plugin.display().to_string()
        );
        assert!(configuration.validation_warnings().is_empty());
        assert_eq!(
            configuration.configuration(Some(""))["typescript.tsdk"],
            temp.path()
                .join("node_modules/typescript/lib")
                .display()
                .to_string()
        );
        assert_eq!(configuration.configuration(Some("vtsls")), options["vtsls"]);
        assert!(configuration.configuration(Some("eslint")).is_null());
        Ok(())
    }

    #[test]
    fn definition_survives_custom_ids_absolute_commands_and_wrappers() {
        for (id, executable) in [
            ("custom", "/opt/bin/vtsls"),
            ("vue", "/opt/custom-wrapper"),
            ("custom", "vtsls.cmd"),
        ] {
            let server: LspServerConfig = serde_json::from_value(json!({
                "id": id, "command": { "command": executable, "arguments": [] }
            }))
            .unwrap();
            assert!(server
                .behavior()
                .default_configuration(Some("vtsls"))
                .is_some());
            assert!(server
                .behavior()
                .error_hint("Cannot find provider for definition")
                .unwrap()
                .contains("@vue/typescript-plugin"));
            assert!(server
                .behavior()
                .error_hint("The \"path\" argument must be of type string")
                .is_none());
        }
    }
}
