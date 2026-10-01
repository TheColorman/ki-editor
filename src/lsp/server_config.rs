use std::path::{Path, PathBuf};

use shared::absolute_path::AbsolutePath;

pub fn resolve_initialization_options(
    value: Option<serde_json::Value>,
    root: &AbsolutePath,
) -> Option<serde_json::Value> {
    value.map(|value| resolve_value(value, root))
}

pub fn resolve_configured_path(root: &AbsolutePath, path: &str) -> Option<PathBuf> {
    if path == "${vue_typescript_plugin}" {
        return vue_typescript_plugin_location(root);
    }

    let path = path.replace("${workspace}", root.as_ref().to_string_lossy().as_ref());
    let path = PathBuf::from(path);
    Some(if path.is_absolute() {
        path
    } else {
        root.as_ref().join(path)
    })
}

fn resolve_value(value: serde_json::Value, root: &AbsolutePath) -> serde_json::Value {
    match value {
        serde_json::Value::String(value) => resolve_string(&value, root),
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .into_iter()
                .map(|value| resolve_value(value, root))
                .collect(),
        ),
        serde_json::Value::Object(values) => serde_json::Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, resolve_value(value, root)))
                .collect(),
        ),
        value => value,
    }
}

fn resolve_string(value: &str, root: &AbsolutePath) -> serde_json::Value {
    if value == "${vue_typescript_plugin}" {
        if let Some(path) = vue_typescript_plugin_location(root) {
            return serde_json::Value::String(path.display().to_string());
        }
        return serde_json::Value::String(value.to_string());
    }
    serde_json::Value::String(
        value.replace("${workspace}", root.as_ref().to_string_lossy().as_ref()),
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
        let value: serde_json::Value = serde_json::from_str(&content).ok()?;
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

    #[test]
    fn resolves_workspace_placeholder() {
        let root: AbsolutePath = "/tmp/ki-workspace".try_into().unwrap();
        let value = serde_json::json!({ "path": "${workspace}/node_modules/typescript/lib" });

        assert_eq!(
            resolve_initialization_options(Some(value), &root).unwrap()["path"],
            "/tmp/ki-workspace/node_modules/typescript/lib"
        );
    }
}
