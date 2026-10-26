use super::*;
use crate::{
    frontend::{mock::MockFrontend, NullWriter},
    lsp::diagnostics::{DiagnosticPublication, DiagnosticSource},
    quickfix_list::DiagnosticSeverityRange,
};
use lsp_types::{Diagnostic, Position, PositionEncodingKind, PublishDiagnosticsParams, Range};

fn app() -> anyhow::Result<(App<MockFrontend>, tempfile::TempDir)> {
    let temp = tempfile::tempdir()?;
    let app = App::new(
        Rc::new(Mutex::new(MockFrontend::new(Box::new(NullWriter)))),
        temp.path().try_into()?,
        Vec::new(),
        RunTestOptions {
            enable_lsp: true,
            enable_syntax_highlighting: false,
            enable_file_watcher: false,
        },
    )?;
    Ok((app, temp))
}

fn publication(path: &AbsolutePath, diagnostics: Vec<Diagnostic>) -> DiagnosticPublication {
    DiagnosticPublication {
        source: DiagnosticSource::External("test".into()),
        params: PublishDiagnosticsParams {
            uri: path.to_url().unwrap(),
            version: None,
            diagnostics,
        },
        encoding: PositionEncodingKind::UTF16,
        lifetime: crate::lsp::diagnostics::DiagnosticLifetime::default(),
    }
}

fn diagnostic() -> Diagnostic {
    Diagnostic::new_simple(
        Range::new(Position::new(0, 1), Position::new(0, 2)),
        "problem".into(),
    )
}

#[test]
fn diagnostic_flood_never_opens_files_or_starts_servers() -> anyhow::Result<()> {
    let (mut app, temp) = app()?;
    let outside = tempfile::tempdir()?;
    let path: AbsolutePath = outside.path().join("GeneratedNamespace.cs").try_into()?;
    std::fs::write(&path, "abc")?;
    let (sender, receiver) = std::sync::mpsc::channel();
    app.syntax_highlight_request_sender = Some(sender);
    for _ in 0..10_000 {
        assert!(!app.publish_diagnostics(publication(&path, Vec::new()))?);
    }
    assert!(app.publish_diagnostics(publication(&path, vec![diagnostic()]))?);
    assert_eq!(
        app.diagnostic_store
            .quickfix_items(DiagnosticSeverityRange::All)
            .len(),
        1
    );
    assert!(app.layout.buffers().is_empty());
    assert!(app
        .lsp_manager
        .values()
        .all(|manager| manager.server_count() == 0));
    assert!(receiver.try_recv().is_err());
    assert!(app.publish_diagnostics(publication(&path, Vec::new()))?);
    assert!(app
        .diagnostic_store
        .quickfix_items(DiagnosticSeverityRange::All)
        .is_empty());
    assert!(temp.path().exists());
    Ok(())
}

#[test]
fn navigating_diagnostics_opens_only_the_selected_file() -> anyhow::Result<()> {
    let (mut app, temp) = app()?;
    app.enable_lsp = false;
    let paths = (0..3)
        .map(|i| -> anyhow::Result<AbsolutePath> {
            let path = temp.path().join(format!("{i}.cs"));
            std::fs::write(&path, "abc")?;
            path.try_into()
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    for path in &paths {
        app.publish_diagnostics(publication(path, vec![diagnostic()]))?;
    }
    assert_eq!(
        app.update_quickfix_list_item(QuickfixListType::Diagnostic(DiagnosticSeverityRange::All)),
        3
    );
    assert!(app.layout.buffers().is_empty());
    app.goto_quickfix_list_item(Movement::Current(IfCurrentNotFound::LookForward))?;
    assert_eq!(app.layout.buffers().len(), 1);
    assert_eq!(app.layout.buffers()[0].borrow().diagnostics().len(), 1);
    assert_eq!(
        app.diagnostic_store
            .quickfix_items(DiagnosticSeverityRange::All)
            .len(),
        2
    );
    app.publish_diagnostics(publication(&paths[0], Vec::new()))?;
    assert!(app.layout.buffers()[0].borrow().diagnostics().is_empty());
    Ok(())
}

#[test]
fn saved_edit_adjusted_diagnostics_survive_close_and_reopen() -> anyhow::Result<()> {
    let (mut app, temp) = app()?;
    app.enable_lsp = false;
    let path: AbsolutePath = temp.path().join("file.txt").try_into()?;
    std::fs::write(&path, "abc\n")?;
    app.open_file(&path, BufferOwner::User, false, true)?;
    app.publish_diagnostics(publication(&path, vec![diagnostic()]))?;
    app.handle_dispatch(Dispatch::ToEditor(DispatchEditor::Insert(
        "prefix\n".into(),
    )))?;
    app.handle_dispatch(Dispatch::ToEditor(DispatchEditor::SaveWithoutFormatting))?;
    let expected = app
        .current_component()
        .borrow()
        .editor()
        .buffer()
        .diagnostics()[0]
        .range;
    app.close_current_window()?;
    assert!(app.layout.buffers().is_empty());
    app.open_file(&path, BufferOwner::User, false, true)?;
    assert_eq!(
        app.current_component()
            .borrow()
            .editor()
            .buffer()
            .diagnostics()[0]
            .range,
        expected
    );
    Ok(())
}

#[test]
fn stale_versions_cannot_clear_current_open_buffer_diagnostics() -> anyhow::Result<()> {
    let (mut app, temp) = app()?;
    app.enable_lsp = false;
    let path: AbsolutePath = temp.path().join("file.txt").try_into()?;
    std::fs::write(&path, "abc")?;
    app.open_file(&path, BufferOwner::User, false, true)?;
    app.publish_diagnostics(publication(&path, vec![diagnostic()]))?;
    let mut stale = publication(&path, Vec::new());
    stale.params.version = Some(0);
    assert!(!app.publish_diagnostics(stale)?);
    assert_eq!(
        app.current_component()
            .borrow()
            .editor()
            .buffer()
            .diagnostics()
            .len(),
        1
    );
    Ok(())
}

#[test]
fn discarded_edits_do_not_restore_adjusted_diagnostics_on_reopen() -> anyhow::Result<()> {
    let (mut app, temp) = app()?;
    app.enable_lsp = false;
    let path: AbsolutePath = temp.path().join("file.txt").try_into()?;
    std::fs::write(&path, "abc\n")?;
    app.open_file(&path, BufferOwner::User, false, true)?;
    app.publish_diagnostics(publication(&path, vec![diagnostic()]))?;
    app.handle_dispatch(Dispatch::ToEditor(DispatchEditor::Insert(
        "prefix\n".into(),
    )))?;
    app.close_current_window()?;
    app.open_file(&path, BufferOwner::User, false, true)?;
    assert!(app
        .current_component()
        .borrow()
        .editor()
        .buffer()
        .diagnostics()
        .is_empty());
    Ok(())
}

#[test]
fn deleting_and_renaming_files_invalidates_cached_diagnostics() -> anyhow::Result<()> {
    let (mut app, temp) = app()?;
    app.enable_lsp = false;
    let path: AbsolutePath = temp.path().join("file.txt").try_into()?;
    std::fs::write(&path, "abc")?;
    app.publish_diagnostics(publication(&path, vec![diagnostic()]))?;
    let renamed = temp.path().join("renamed.txt");
    app.move_path(path, renamed.clone())?;
    assert!(app
        .diagnostic_store
        .quickfix_items(DiagnosticSeverityRange::All)
        .is_empty());
    let renamed: AbsolutePath = renamed.try_into()?;
    app.publish_diagnostics(publication(&renamed, vec![diagnostic()]))?;
    app.delete_paths(NonEmpty::new(renamed))?;
    assert!(app
        .diagnostic_store
        .quickfix_items(DiagnosticSeverityRange::All)
        .is_empty());
    Ok(())
}

#[test]
fn retired_server_messages_cannot_repopulate_diagnostics() -> anyhow::Result<()> {
    let (mut app, temp) = app()?;
    let path: AbsolutePath = temp.path().join("file.cs").try_into()?;
    std::fs::write(&path, "abc")?;
    let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let source = DiagnosticSource::server(crate::lsp::manager::LspServerKey {
        language_id: "csharp".into(),
        server_id: "omnisharp".into(),
        root: temp.path().try_into()?,
    });
    let mut update = publication(&path, vec![diagnostic()]);
    update.source = source.clone();
    update.lifetime = crate::lsp::diagnostics::DiagnosticLifetime(alive.clone());
    app.publish_diagnostics(update.clone())?;
    alive.store(false, std::sync::atomic::Ordering::Release);
    app.handle_lsp_notification(LspNotification::DiagnosticsStopped(source))?;
    assert!(!app.publish_diagnostics(update)?);
    assert!(app
        .diagnostic_store
        .quickfix_items(DiagnosticSeverityRange::All)
        .is_empty());
    Ok(())
}
