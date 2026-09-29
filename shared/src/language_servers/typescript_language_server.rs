use crate::language::{Command, LspCommand};

use super::definition::{ConfiguredPath, ServerBehavior, ServerDefinition};
use serde_json::{json, Value};

pub const DEFINITION: ServerDefinition = ServerDefinition {
    ids: &["typescript-language-server"],
    executables: &["typescript-language-server"],
    behavior: &Typescript,
    root_policy: &super::package_workspace::ROOT_POLICY,
};

struct Typescript;

impl ServerBehavior for Typescript {
    fn default_configuration(&self, section: Option<&str>) -> Option<Value> {
        match section {
            Some("") => Some(json!({
                "typescript.tsdk": "${workspace}/node_modules/typescript/lib",
                "typescript.validate.enable": true,
                "javascript.validate.enable": true
            })),
            Some("typescript") => Some(json!({
                "tsdk": "${workspace}/node_modules/typescript/lib",
                "validate": { "enable": true }
            })),
            _ => None,
        }
    }

    fn configured_paths(&self, options: &Value) -> Vec<ConfiguredPath> {
        sdk_paths(options)
    }
}

pub(super) fn sdk_paths(options: &Value) -> Vec<ConfiguredPath> {
    options
        .pointer("/typescript/tsdk")
        .or_else(|| options.pointer("/typescript.tsdk"))
        .and_then(Value::as_str)
        .map(|value| ConfiguredPath {
            label: "TypeScript SDK".to_string(),
            value: value.to_string(),
        })
        .into_iter()
        .collect()
}

pub fn command() -> LspCommand {
    LspCommand {
        command: Command::new("typescript-language-server", &["--stdio"]),
        ..LspCommand::default()
    }
}
