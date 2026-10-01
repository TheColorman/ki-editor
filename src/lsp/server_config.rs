use lsp_types::ConfigurationParams;
use serde_json::Value;
use shared::{absolute_path::AbsolutePath, language::LspServerConfig};

/// Expand workspace-relative paths without changing non-string option values.
pub(super) fn resolve_options(value: Value, root: &AbsolutePath) -> Value {
    match value {
        Value::String(value) => {
            Value::String(value.replace("${workspace}", root.as_ref().to_string_lossy().as_ref()))
        }
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| resolve_options(value, root))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, resolve_options(value, root)))
                .collect(),
        ),
        value => value,
    }
}

pub(super) fn workspace_configuration(
    params: ConfigurationParams,
    config: &LspServerConfig,
    root: &AbsolutePath,
) -> Value {
    Value::Array(
        params
            .items
            .into_iter()
            .map(|item| {
                let settings = config.settings().cloned().unwrap_or(Value::Null);
                let value = match item.section.as_deref() {
                    None | Some("") => settings,
                    Some(section) => settings
                        .get(section)
                        .cloned()
                        .or_else(|| {
                            section
                                .split('.')
                                .try_fold(&settings, |value, key| value.get(key))
                                .cloned()
                        })
                        .unwrap_or(Value::Null),
                };
                resolve_options(value, root)
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::ConfigurationItem;
    use serde_json::json;

    #[test]
    fn configuration_is_scoped_to_the_requesting_server() {
        let root: AbsolutePath = std::env::current_dir().unwrap().try_into().unwrap();
        let config: LspServerConfig = serde_json::from_value(json!({
            "id": "example", "command": {"command": "server", "arguments": []},
            "settings": {"typescript": {"tsdk": "${workspace}/node_modules/typescript/lib"}, "enabled": true}
        })).unwrap();
        let params = || ConfigurationParams {
            items: ["typescript.tsdk", "enabled", "unknown"]
                .into_iter()
                .map(|section| ConfigurationItem {
                    scope_uri: None,
                    section: Some(section.to_string()),
                })
                .collect(),
        };
        assert_eq!(
            workspace_configuration(params(), &config, &root),
            json!([
                format!("{}/node_modules/typescript/lib", root.as_ref().display()),
                true,
                null
            ])
        );
        let other = LspServerConfig::new("other", shared::language::Command::new("other", &[]));
        assert_eq!(
            workspace_configuration(params(), &other, &root),
            json!([null, null, null])
        );
    }
}
