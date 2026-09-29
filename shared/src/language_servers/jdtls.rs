use crate::language::{Command, LspServerConfig};

use super::{
    definition::{GenericServer, ServerDefinition},
    workspace_root::{RootFallback, RootMarker, WorkspaceRootPolicy},
};

pub const DEFINITION: ServerDefinition = ServerDefinition {
    ids: &["jdtls"],
    executables: &["jdtls"],
    behavior: &GenericServer,
    root_policy: &ROOT_POLICY,
};

const ROOT_POLICY: WorkspaceRootPolicy = WorkspaceRootPolicy {
    marker_groups: &[
        &[
            RootMarker::Paths(&[
                "settings.gradle",
                "settings.gradle.kts",
                "gradlew",
                "gradlew.bat",
                "mvnw",
                "mvnw.cmd",
            ]),
            RootMarker::Directories(&[".mvn"]),
        ],
        &[RootMarker::Paths(&[
            "pom.xml",
            "build.gradle",
            "build.gradle.kts",
        ])],
    ],
    fallback: RootFallback::FileParent,
    canonicalize: true,
};

pub fn config() -> LspServerConfig {
    LspServerConfig {
        initialization_options: Some(serde_json::json!({
            "bundles": [],
            "settings": {
                "java": {
                    "signatureHelp": {
                        "enabled": true
                    }
                }
            }
        })),
        ..LspServerConfig::new(
            "jdtls",
            Command::new("jdtls", &["-data", "${lsp_data_dir}"]),
        )
    }
}
