use shared::{
    absolute_path::AbsolutePath, language::LspServerConfig,
    language_servers::configuration::ServerConfiguration, process_command::ProcessCommand,
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
    let data_dir = workspace_data_lease.as_ref().map(|lease| lease.data_dir());
    let configuration = ServerConfiguration::new(server_config, root).with_data_dir(data_dir);
    let resolve = |value: &str| configuration.resolve_string(value);
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

#[cfg(test)]
mod tests {
    use super::*;
    use shared::language::Command;
    use std::path::Path;

    #[test]
    fn resolves_workspace_placeholder() {
        let root: AbsolutePath = "/tmp/ki-workspace".try_into().unwrap();
        let server = LspServerConfig::new("test", Command::new("test-server", &[]));
        let value = serde_json::json!({ "path": "${workspace}/sdk" });

        assert_eq!(
            ServerConfiguration::new(&server, &root).resolve_value(value)["path"],
            "/tmp/ki-workspace/sdk"
        );
    }

    #[test]
    fn resolves_process_command_placeholders() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let root: AbsolutePath = root.path().try_into()?;
        let server: LspServerConfig = serde_json::from_value(serde_json::json!({
            "id": "test",
            "command": {
                "command": "test-server",
                "arguments": ["-data", "${lsp_data_dir}", "--root=${workspace}"]
            },
            "environment": { "PROJECT_ROOT": "${workspace}" }
        }))?;

        let resolved = resolve_process_command(&server, &root)?;

        assert_eq!(resolved.command.command(), "test-server");
        assert_eq!(resolved.command.arguments()[0], "-data");
        assert!(Path::new(&resolved.command.arguments()[1]).is_absolute());
        assert_eq!(
            resolved.command.arguments()[2],
            format!("--root={}", root.display_absolute())
        );
        assert_eq!(
            resolved.command.environment()["PROJECT_ROOT"],
            root.display_absolute()
        );
        assert!(resolved.workspace_data_lease.is_some());
        Ok(())
    }

    #[test]
    fn process_command_without_data_placeholder_does_not_allocate_lease() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let root: AbsolutePath = root.path().try_into()?;
        let server = LspServerConfig::new("test", Command::new("test-server", &[]));

        let resolved = resolve_process_command(&server, &root)?;

        assert!(resolved.workspace_data_lease.is_none());
        Ok(())
    }

    #[test]
    fn placeholder_values_are_not_reprocessed() {
        let server = LspServerConfig::new("test", Command::new("test-server", &[]));
        let root: AbsolutePath = "/tmp/${lsp_data_dir}/project".try_into().unwrap();
        assert_eq!(
            ServerConfiguration::new(&server, &root)
                .with_data_dir(Some(Path::new("/cache/server")))
                .resolve_string("${workspace}"),
            "/tmp/${lsp_data_dir}/project"
        );
    }
}
