use super::super::*;

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
