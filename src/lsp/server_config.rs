use lsp_types::ConfigurationParams;
use serde_json::Value;
use shared::{absolute_path::AbsolutePath, language::LspServerConfig};

pub(super) fn root_for_path(
    config: &LspServerConfig,
    path: &AbsolutePath,
    boundary: &AbsolutePath,
) -> AbsolutePath {
    let parent = path.as_ref().parent().unwrap_or(path.as_ref());
    config
        .root_markers()
        .iter()
        .find_map(|markers| {
            parent
                .ancestors()
                .take_while(|ancestor| ancestor.starts_with(boundary.as_ref()))
                .find(|ancestor| markers.iter().any(|marker| ancestor.join(marker).exists()))
                .and_then(|path| path.try_into().ok())
        })
        .unwrap_or_else(|| boundary.clone())
}

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
    fn root_markers_respect_priority_and_boundary() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let root: AbsolutePath = tempdir.path().try_into()?;
        let app = tempdir.path().join("frontend/apps/example");
        std::fs::create_dir_all(&app)?;
        std::fs::write(
            tempdir.path().join("frontend/pnpm-workspace.yaml"),
            "packages: []",
        )?;
        std::fs::write(app.join("package.json"), "{}")?;
        let path: AbsolutePath = app.join("example.vue").try_into()?;
        let config: LspServerConfig = serde_json::from_value(json!({
            "id": "example", "command": {"command": "server", "arguments": []},
            "root_markers": [["pnpm-workspace.yaml"], ["package.json"]]
        }))?;
        assert_eq!(
            root_for_path(&config, &path, &root).as_ref(),
            tempdir.path().join("frontend")
        );
        let boundary: AbsolutePath = app.as_path().try_into()?;
        assert_eq!(root_for_path(&config, &path, &boundary), boundary);
        let legacy = LspServerConfig::new("legacy", shared::language::Command::new("server", &[]));
        assert_eq!(root_for_path(&legacy, &path, &root), root);
        Ok(())
    }

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
