use std::path::Path;

use serde_json::Value;

use super::workspace_root::WorkspaceRootPolicy;
use crate::{absolute_path::AbsolutePath, language::LspServerConfig};

/// Runtime policy supplied by a server module. The LSP transport and manager
/// depend on this interface, never on a particular server or language.
pub trait ServerBehavior: Sync {
    fn default_configuration(&self, _section: Option<&str>) -> Option<Value> {
        None
    }

    fn placeholders(&self, _root: &AbsolutePath) -> Vec<Placeholder> {
        Vec::new()
    }

    fn configured_paths(&self, _options: &Value) -> Vec<ConfiguredPath> {
        Vec::new()
    }

    fn error_hint(&self, _message: &str) -> Option<&'static str> {
        None
    }
}

pub struct Placeholder {
    pub name: &'static str,
    /// `None` leaves the token intact in settings and reports an unresolved path
    /// during validation. It must not become a literal path under the workspace.
    pub value: Option<String>,
}

pub struct ConfiguredPath {
    pub label: String,
    pub value: String,
}

/// Identifies a server's policy independently of its user-configured executable
/// location. IDs support wrapper commands; executable names support custom IDs.
pub struct ServerDefinition {
    pub ids: &'static [&'static str],
    pub executables: &'static [&'static str],
    pub behavior: &'static dyn ServerBehavior,
    pub root_policy: &'static WorkspaceRootPolicy,
}

impl ServerDefinition {
    pub(super) fn matches(&self, config: &LspServerConfig) -> bool {
        let command = config.process_command();
        let executable = Path::new(command.command())
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(command.command());
        let executable = executable
            .strip_suffix(".exe")
            .or_else(|| executable.strip_suffix(".cmd"))
            .unwrap_or(executable);
        self.ids.contains(&config.id()) || self.executables.contains(&executable)
    }
}

pub(super) struct GenericServer;
impl ServerBehavior for GenericServer {}
