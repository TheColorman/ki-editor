use std::path::{Path, PathBuf};

use shared::{
    absolute_path::AbsolutePath, language::LspServerConfig, process_command::ProcessCommand,
};

use super::workspace_data::WorkspaceDataLease;

pub struct ResolvedProcessCommand {
    pub command: ProcessCommand,
    pub workspace_data_lease: Option<WorkspaceDataLease>,
}

pub fn resolve_process_command(
    server_config: &LspServerConfig,
    root: &AbsolutePath,
) -> anyhow::Result<ResolvedProcessCommand> {
    let command = server_config.process_command();
    let needs_data_dir = std::iter::once(command.command())
        .chain(command.arguments().iter().map(String::as_str))
        .chain(command.environment().values().map(String::as_str))
        .any(|value| value.contains("${lsp_data_dir}"));
    let workspace_data_lease = needs_data_dir
        .then(|| {
            WorkspaceDataLease::acquire(
                grammar::cache_dir().as_path(),
                server_config.id(),
                root.as_ref(),
            )
        })
        .transpose()?;
    let data_dir = workspace_data_lease
        .as_ref()
        .map(|lease| lease.data_dir().to_string_lossy().into_owned());
    let workspace = root.as_ref().to_string_lossy();
    let resolve = |value: &str| resolve_launch_placeholders(value, &workspace, data_dir.as_deref());
    let resolved_command = resolve(command.command());
    let resolved_arguments = command
        .arguments()
        .iter()
        .map(|argument| resolve(argument))
        .collect::<Vec<_>>();
    let resolved_environment = command
        .environment()
        .iter()
        .map(|(key, value)| (key.clone(), resolve(value)))
        .collect();

    Ok(ResolvedProcessCommand {
        command: ProcessCommand::with_environment(
            &resolved_command,
            &resolved_arguments,
            &resolved_environment,
        ),
        workspace_data_lease,
    })
}

fn resolve_launch_placeholders(value: &str, workspace: &str, data_dir: Option<&str>) -> String {
    let replacements = [
        ("${workspace}", workspace),
        ("${lsp_data_dir}", data_dir.unwrap_or("${lsp_data_dir}")),
    ];
    let mut result = String::with_capacity(value.len());
    let mut remaining = value;

    while let Some((index, token, replacement)) = replacements
        .iter()
        .filter_map(|(token, replacement)| {
            remaining
                .find(token)
                .map(|index| (index, *token, *replacement))
        })
        .min_by_key(|(index, _, _)| *index)
    {
        result.push_str(&remaining[..index]);
        result.push_str(replacement);
        remaining = &remaining[index + token.len()..];
    }
    result.push_str(remaining);
    result
}

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
    use shared::language::Command;

    #[test]
    fn resolves_workspace_placeholder() {
        let root: AbsolutePath = "/tmp/ki-workspace".try_into().unwrap();
        let value = serde_json::json!({ "path": "${workspace}/node_modules/typescript/lib" });

        assert_eq!(
            resolve_initialization_options(Some(value), &root).unwrap()["path"],
            "/tmp/ki-workspace/node_modules/typescript/lib"
        );
    }

    #[test]
    fn resolves_process_command_placeholders() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let root: AbsolutePath = root.path().try_into()?;
        let server: LspServerConfig = serde_json::from_value(serde_json::json!({
            "id": "jdtls",
            "command": {
                "command": "jdtls",
                "arguments": ["-data", "${lsp_data_dir}", "--root=${workspace}"]
            },
            "environment": { "JAVA_PROJECT": "${workspace}" }
        }))?;

        let resolved = resolve_process_command(&server, &root)?;

        assert_eq!(resolved.command.command(), "jdtls");
        assert_eq!(resolved.command.arguments()[0], "-data");
        assert!(Path::new(&resolved.command.arguments()[1]).is_absolute());
        assert_eq!(
            resolved.command.arguments()[2],
            format!("--root={}", root.display_absolute())
        );
        assert_eq!(
            resolved.command.environment()["JAVA_PROJECT"],
            root.display_absolute()
        );
        assert!(resolved.workspace_data_lease.is_some());
        Ok(())
    }

    #[test]
    fn process_command_without_data_placeholder_does_not_allocate_lease() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let root: AbsolutePath = root.path().try_into()?;
        let server = LspServerConfig::new("rust", Command::new("rust-analyzer", &[]));

        let resolved = resolve_process_command(&server, &root)?;

        assert!(resolved.workspace_data_lease.is_none());
        Ok(())
    }

    #[test]
    fn placeholder_values_are_not_reprocessed() {
        assert_eq!(
            resolve_launch_placeholders(
                "${workspace}",
                "/tmp/${lsp_data_dir}/project",
                Some("/cache/jdtls")
            ),
            "/tmp/${lsp_data_dir}/project"
        );
    }
}
