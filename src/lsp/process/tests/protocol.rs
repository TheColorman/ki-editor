use super::super::*;

#[test]
fn pull_diagnostics_keep_document_versions_and_ignore_obsolete_responses() -> anyhow::Result<()> {
    let (mut process, mut child, app_receiver, _, _, _temp, _) =
        super::support::process_for_server_requests()?;
    let path: AbsolutePath = std::env::current_dir()?.join("file.cs").try_into()?;
    process.synchronized_documents.insert(
        path.clone(),
        SynchronizedDocument {
            content: "abc".into(),
            version: 1,
        },
    );
    let request = DocumentDiagnosticParams {
        text_document: TextDocumentIdentifier {
            uri: path.to_url().unwrap(),
        },
        identifier: None,
        previous_result_id: None,
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    process.send_request::<lsp_request!("textDocument/diagnostic")>(
        ResponseContext::default(),
        Some(path.clone()),
        request.clone(),
    )?;
    process
        .synchronized_documents
        .get_mut(&path)
        .unwrap()
        .version = 2;
    process.handle_reply(
        serde_json::json!({ "jsonrpc": "2.0", "id": 0, "result": { "kind": "full", "items": [] } }),
    )?;
    assert!(app_receiver.try_recv().is_err());
    process.send_request::<lsp_request!("textDocument/diagnostic")>(
        ResponseContext::default(),
        Some(path),
        request,
    )?;
    process.handle_reply(
        serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": { "kind": "full", "items": [] } }),
    )?;
    let AppMessage::DiagnosticsReady(inbox) = app_receiver.try_recv()? else {
        panic!("Expected diagnostics");
    };
    let (publications, more) = inbox.take_batch(64);
    assert!(!more);
    let publication = &publications[0];
    assert_eq!(publication.params.version, Some(2));
    assert_eq!(publication.encoding, PositionEncodingKind::UTF16);
    drop(process);
    child.wait()?;
    Ok(())
}
use super::support::{process_for_server_requests, read_json_rpc_messages};

#[test]
fn workspace_file_notifications_require_server_file_operation_capabilities() {
    let mut capabilities = ServerCapabilities::default();

    assert!(!LspServerProcess::server_supports_workspace_did_create_files(&capabilities));
    assert!(!LspServerProcess::server_supports_workspace_did_rename_files(&capabilities));

    capabilities.workspace = Some(WorkspaceServerCapabilities {
        file_operations: Some(WorkspaceFileOperationsServerCapabilities {
            did_create: Some(FileOperationRegistrationOptions { filters: vec![] }),
            did_rename: Some(FileOperationRegistrationOptions { filters: vec![] }),
            ..WorkspaceFileOperationsServerCapabilities::default()
        }),
        ..WorkspaceServerCapabilities::default()
    });

    assert!(LspServerProcess::server_supports_workspace_did_create_files(&capabilities));
    assert!(LspServerProcess::server_supports_workspace_did_rename_files(&capabilities));
}

#[test]
fn full_document_diagnostic_report_returns_diagnostics() {
    let diagnostic = lsp_types::Diagnostic::new_simple(
        lsp_types::Range::new(
            lsp_types::Position::new(0, 0),
            lsp_types::Position::new(0, 1),
        ),
        "hello".to_string(),
    );
    let result = DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(
        RelatedFullDocumentDiagnosticReport {
            related_documents: None,
            full_document_diagnostic_report: FullDocumentDiagnosticReport {
                result_id: None,
                items: vec![diagnostic.clone()],
            },
        },
    ));

    assert_eq!(
        diagnostics_from_document_diagnostic(result),
        Some(vec![diagnostic])
    );
}

#[test]
fn initialize_advertises_code_action_resolve_support() -> anyhow::Result<()> {
    let (mut process, mut child, _app_receiver, _sender, _receiver, _tempdir, messages_path) =
        process_for_server_requests()?;

    process.initialize()?;
    drop(process);
    child.wait()?;

    let messages = read_json_rpc_messages(&messages_path)?;
    assert_eq!(
        messages[0]["params"]["capabilities"]["textDocument"]["codeAction"]["dataSupport"],
        true
    );
    assert_eq!(
        messages[0]["params"]["capabilities"]["textDocument"]["codeAction"]["resolveSupport"]
            ["properties"],
        serde_json::json!(["edit"])
    );
    Ok(())
}

#[test]
fn code_action_resolve_preserves_server_data() -> anyhow::Result<()> {
    let (mut process, mut child, app_receiver, _sender, _receiver, _tempdir, messages_path) =
        process_for_server_requests()?;
    process.server_capabilities = Some(ServerCapabilities {
        code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
            resolve_provider: Some(true),
            ..Default::default()
        })),
        ..Default::default()
    });
    let path: AbsolutePath = std::env::current_dir()?.join("Program.cs").try_into()?;
    let uri = path.to_url().unwrap();
    let data = serde_json::json!({
        "Identifier": "using System.Text.Json;",
        "$$__handler_id__$$": "handler"
    });

    process.code_action_resolve(
        RequestParams {
            path,
            position: crate::position::Position::new(0, 18),
            selection_end: crate::position::Position::new(0, 32),
            context: ResponseContext::default(),
        },
        lsp_types::CodeAction {
            title: "using System.Text.Json;".to_string(),
            kind: Some(CodeActionKind::QUICKFIX),
            data: Some(data.clone()),
            ..Default::default()
        },
    )?;
    process.handle_reply(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 0,
        "result": {
            "title": "using System.Text.Json;",
            "kind": "quickfix",
            "edit": {
                "changes": {
                    (uri.as_str()): [{
                        "range": {
                            "start": { "line": 0, "character": 0 },
                            "end": { "line": 0, "character": 0 }
                        },
                        "newText": "using System.Text.Json;\n\n"
                    }]
                }
            },
            "data": data
        }
    }))?;
    let AppMessage::LspNotification(notification) = app_receiver.recv()? else {
        panic!("expected an LSP notification");
    };
    let LspNotification::CodeActionResolve(action) = *notification else {
        panic!("expected a resolved code action");
    };
    assert!(action.edit.is_some());
    drop(process);
    child.wait()?;

    let messages = read_json_rpc_messages(&messages_path)?;
    assert_eq!(messages[0]["method"], "codeAction/resolve");
    assert_eq!(messages[0]["params"]["data"], data);
    Ok(())
}
