//! Language-agnostic workspace-root discovery. Server modules supply marker
//! groups in priority order; this module owns traversal and filesystem matching.

use std::{
    cell::OnceCell,
    path::{Path, PathBuf},
};

/// A match succeeds when any of the supplied names or extensions is present.
/// Named paths retain `Path::exists` semantics, including directory entries.
#[derive(Debug, Clone, Copy)]
pub enum RootMarker {
    Paths(&'static [&'static str]),
    Directories(&'static [&'static str]),
    /// Matches entry extensions case-insensitively, without a leading dot.
    Extensions(&'static [&'static str]),
}

#[derive(Debug, Clone, Copy)]
pub enum RootFallback {
    Boundary,
    FileParent,
}

#[derive(Debug, Clone, Copy)]
pub struct WorkspaceRootPolicy {
    /// Earlier groups outrank later groups, even when their match is farther
    /// away. Within a group, the nearest matching ancestor wins.
    pub marker_groups: &'static [&'static [RootMarker]],
    pub fallback: RootFallback,
    /// Canonicalize the selected root when possible, preserving it on failure.
    pub canonicalize: bool,
}

impl WorkspaceRootPolicy {
    /// Search up from the file's directory, including the boundary if encountered.
    /// When the file is outside the boundary, search its actual ancestors instead.
    pub fn resolve(&self, file_parent: &Path, boundary: &Path) -> PathBuf {
        let mut best: Option<(usize, &Path)> = None;
        for ancestor in file_parent.ancestors() {
            let candidate = RootCandidate::new(ancestor);
            // A farther match at the same priority cannot improve the result.
            let better_groups = best.map_or(self.marker_groups.len(), |(priority, _)| priority);
            if let Some(priority) = self
                .marker_groups
                .iter()
                .take(better_groups)
                .position(|markers| markers.iter().any(|marker| candidate.matches(marker)))
            {
                best = Some((priority, ancestor));
                if priority == 0 {
                    break;
                }
            }
            if ancestor == boundary {
                break;
            }
        }

        let root = best
            .map(|(_, path)| path)
            .unwrap_or(match self.fallback {
                RootFallback::Boundary => boundary,
                RootFallback::FileParent => file_parent,
            })
            .to_path_buf();
        if self.canonicalize {
            root.canonicalize().unwrap_or(root)
        } else {
            root
        }
    }
}

struct RootCandidate<'a> {
    path: &'a Path,
    /// Extension groups share one directory listing, loaded only if needed.
    extensions: OnceCell<Vec<String>>,
}

impl<'a> RootCandidate<'a> {
    fn new(path: &'a Path) -> Self {
        Self {
            path,
            extensions: OnceCell::new(),
        }
    }

    fn matches(&self, marker: &RootMarker) -> bool {
        match marker {
            RootMarker::Paths(names) => names.iter().any(|name| self.path.join(name).exists()),
            RootMarker::Directories(names) => {
                names.iter().any(|name| self.path.join(name).is_dir())
            }
            RootMarker::Extensions(extensions) => self
                .extensions
                .get_or_init(|| {
                    std::fs::read_dir(self.path)
                        .into_iter()
                        .flatten()
                        .filter_map(Result::ok)
                        .filter_map(|entry| {
                            entry
                                .path()
                                .extension()
                                .and_then(|extension| extension.to_str())
                                .map(str::to_owned)
                        })
                        .collect()
                })
                .iter()
                .any(|extension| {
                    extensions
                        .iter()
                        .any(|candidate| extension.eq_ignore_ascii_case(candidate))
                }),
        }
    }
}

#[cfg(test)]
mod tests;
