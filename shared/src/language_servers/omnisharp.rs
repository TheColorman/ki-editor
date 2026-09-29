use crate::language::{Command, LspCommand};

use super::{
    definition::{GenericServer, ServerDefinition},
    package_workspace,
    workspace_root::{RootFallback, RootMarker, WorkspaceRootPolicy},
};

pub const DEFINITION: ServerDefinition = ServerDefinition {
    ids: &["omnisharp"],
    executables: &["omnisharp", "OmniSharp"],
    behavior: &GenericServer,
    root_policy: &ROOT_POLICY,
};

const ROOT_POLICY: WorkspaceRootPolicy = WorkspaceRootPolicy {
    marker_groups: &[
        &[RootMarker::Extensions(&["sln", "slnx"])],
        &[RootMarker::Extensions(&["csproj"])],
        // These markers share a priority: the nearest package of either kind wins.
        &[
            package_workspace::WORKSPACE_MARKERS,
            package_workspace::PROJECT_MARKERS,
        ],
    ],
    fallback: RootFallback::Boundary,
    canonicalize: false,
};

pub fn command() -> LspCommand {
    LspCommand {
        command: Command::new("omnisharp", &["--languageserver"]),
        ..LspCommand::default()
    }
}
