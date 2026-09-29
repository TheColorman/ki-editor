//! Reusable package markers and the default server root policy.

use super::workspace_root::{RootFallback, RootMarker, WorkspaceRootPolicy};

pub(super) const WORKSPACE_MARKERS: RootMarker = RootMarker::Paths(&[
    "pnpm-workspace.yaml",
    "pnpm-workspace.yml",
    "pnpm-lock.yaml",
    "yarn.lock",
    "package-lock.json",
    "bun.lockb",
    "bun.lock",
]);

pub(super) const PROJECT_MARKERS: RootMarker = RootMarker::Paths(&["package.json"]);

pub(super) const ROOT_POLICY: WorkspaceRootPolicy = WorkspaceRootPolicy {
    marker_groups: &[&[WORKSPACE_MARKERS], &[PROJECT_MARKERS]],
    fallback: RootFallback::Boundary,
    canonicalize: false,
};
