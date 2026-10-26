//! Workspace diagnostics must not own editors, source text, or language servers.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::SystemTime,
};

use lsp_types::{Diagnostic, PositionEncodingKind, PublishDiagnosticsParams};
use shared::absolute_path::AbsolutePath;

use super::{manager::LspServerKey, position::EncodedRange};
use crate::{
    components::suggestive_editor::Info,
    quickfix_list::{DiagnosticSeverityRange, QuickfixListItem},
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DiagnosticSource {
    External(String),
    Server {
        server: LspServerKey,
        session: uuid::Uuid,
    },
}

impl DiagnosticSource {
    pub fn server(server: LspServerKey) -> Self {
        Self::Server {
            server,
            session: uuid::Uuid::new_v4(),
        }
    }
}

/// Retirement is separate from source identity: hash-map keys never mutate.
#[derive(Debug, Clone)]
pub struct DiagnosticLifetime(pub Arc<AtomicBool>);

impl DiagnosticLifetime {
    pub fn is_active(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

impl Default for DiagnosticLifetime {
    fn default() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }
}
impl PartialEq for DiagnosticLifetime {
    fn eq(&self, other: &Self) -> bool {
        self.is_active() == other.is_active()
    }
}
impl Eq for DiagnosticLifetime {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticSnapshot {
    pub diagnostics: Vec<Diagnostic>,
    pub version: Option<i32>,
    pub encoding: PositionEncodingKind,
    pub lifetime: DiagnosticLifetime,
}

impl DiagnosticSnapshot {
    pub fn external(diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            diagnostics,
            version: None,
            encoding: PositionEncodingKind::UTF32,
            lifetime: DiagnosticLifetime::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiagnosticPublication {
    pub source: DiagnosticSource,
    pub params: PublishDiagnosticsParams,
    pub encoding: PositionEncodingKind,
    pub lifetime: DiagnosticLifetime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredDiagnostics {
    snapshot: DiagnosticSnapshot,
    modified: Option<SystemTime>,
}

/// Only unopened files live here. Open buffers own their edit-adjusted ranges.
#[derive(Default)]
pub struct DiagnosticStore {
    documents: HashMap<AbsolutePath, HashMap<DiagnosticSource, StoredDiagnostics>>,
}

impl DiagnosticStore {
    pub fn replace(
        &mut self,
        path: AbsolutePath,
        source: DiagnosticSource,
        snapshot: DiagnosticSnapshot,
    ) -> bool {
        if !snapshot.lifetime.is_active() {
            return false;
        }
        if self.documents.get(&path).and_then(|sources| sources.get(&source)).is_some_and(|old| {
            matches!((old.snapshot.version, snapshot.version), (Some(old), Some(new)) if new < old)
        }) {
            return false;
        }
        if snapshot.diagnostics.is_empty() {
            let changed = self
                .documents
                .get_mut(&path)
                .is_some_and(|sources| sources.remove(&source).is_some());
            if self.documents.get(&path).is_some_and(HashMap::is_empty) {
                self.documents.remove(&path);
            }
            return changed;
        }
        let stored = StoredDiagnostics {
            snapshot,
            modified: path.last_modified_time().ok(),
        };
        let sources = self.documents.entry(path).or_default();
        if sources.get(&source) == Some(&stored) {
            return false;
        }
        sources.insert(source, stored);
        true
    }

    pub fn take(&mut self, path: &AbsolutePath) -> HashMap<DiagnosticSource, DiagnosticSnapshot> {
        let modified = path.last_modified_time().ok();
        self.documents
            .remove(path)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(source, stored)| {
                (stored.snapshot.lifetime.is_active() && stored.modified == modified).then_some({
                    // A newly opened buffer starts a new document-version sequence.
                    (
                        source,
                        DiagnosticSnapshot {
                            version: None,
                            ..stored.snapshot
                        },
                    )
                })
            })
            .collect()
    }

    pub fn clear_source(&mut self, source: &DiagnosticSource) {
        self.documents.retain(|_, sources| {
            sources.remove(source);
            !sources.is_empty()
        });
    }

    pub fn remove_path(&mut self, path: &AbsolutePath) {
        self.documents
            .retain(|document, _| !path.is_parent_of(document));
    }

    pub fn quickfix_items(&self, severity: DiagnosticSeverityRange) -> Vec<QuickfixListItem> {
        self.documents
            .iter()
            .flat_map(|(path, sources)| {
                sources
                    .iter()
                    .filter(|(_, stored)| stored.snapshot.lifetime.is_active())
                    .flat_map(|(_, stored)| {
                        stored
                            .snapshot
                            .diagnostics
                            .iter()
                            .filter(|diagnostic| severity.contains(diagnostic.severity))
                            .map(|diagnostic| {
                                QuickfixListItem::document(
                                    path.clone(),
                                    EncodedRange {
                                        range: diagnostic.range,
                                        encoding: stored.snapshot.encoding.clone(),
                                    },
                                    Some(Info::new(
                                        "Diagnostics".to_string(),
                                        diagnostic.message.clone(),
                                    )),
                                )
                            })
                    })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_publications_do_not_accumulate_documents() -> anyhow::Result<()> {
        let mut store = DiagnosticStore::default();
        for i in 0..10_000 {
            assert!(!store.replace(
                format!("/unopened/{i}.cs").try_into()?,
                DiagnosticSource::External("test".into()),
                DiagnosticSnapshot::external(Vec::new())
            ));
        }
        assert!(store.documents.is_empty());
        Ok(())
    }

    #[test]
    fn clearing_one_root_does_not_clear_another_servers_diagnostics() -> anyhow::Result<()> {
        let path: AbsolutePath = "/unopened/shared.cs".try_into()?;
        let source = |root: &str| -> anyhow::Result<_> {
            Ok(DiagnosticSource::server(LspServerKey {
                language_id: "csharp".into(),
                server_id: "omnisharp".into(),
                root: root.try_into()?,
            }))
        };
        let first = source("/first")?;
        let second = source("/second")?;
        let mut store = DiagnosticStore::default();
        let snapshot = DiagnosticSnapshot::external(vec![Diagnostic::new_simple(
            lsp_types::Range::default(),
            "error".into(),
        )]);
        store.replace(path.clone(), first.clone(), snapshot.clone());
        store.replace(path.clone(), second, snapshot);
        store.replace(path, first, DiagnosticSnapshot::external(Vec::new()));
        assert_eq!(store.quickfix_items(DiagnosticSeverityRange::All).len(), 1);
        Ok(())
    }

    #[test]
    fn stale_versioned_clears_do_not_remove_a_newer_snapshot() -> anyhow::Result<()> {
        let path: AbsolutePath = "/unopened/shared.cs".try_into()?;
        let source = DiagnosticSource::External("test".into());
        let mut store = DiagnosticStore::default();
        let mut snapshot = DiagnosticSnapshot::external(vec![Diagnostic::default()]);
        snapshot.version = Some(5);
        store.replace(path.clone(), source.clone(), snapshot);
        let mut clear = DiagnosticSnapshot::external(Vec::new());
        clear.version = Some(4);
        assert!(!store.replace(path, source, clear));
        assert_eq!(store.quickfix_items(DiagnosticSeverityRange::All).len(), 1);
        Ok(())
    }

    #[test]
    fn reopening_drops_snapshots_for_replaced_disk_contents() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let path: AbsolutePath = temp.path().join("file.cs").try_into()?;
        std::fs::write(&path, "old")?;
        let mut store = DiagnosticStore::default();
        store.replace(
            path.clone(),
            DiagnosticSource::External("test".into()),
            DiagnosticSnapshot::external(vec![Diagnostic::new_simple(
                lsp_types::Range::default(),
                "error".into(),
            )]),
        );
        // A removed file cannot hydrate a stale snapshot, independently of mtime resolution.
        std::fs::remove_file(&path)?;
        assert!(store.take(&path).is_empty());
        Ok(())
    }
}
