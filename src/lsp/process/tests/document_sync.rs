use super::super::*;
use super::support::{process_for_server_requests, read_json_rpc_messages, sync_capabilities};

#[test]
fn text_document_lifecycle_notifications_require_server_sync_capabilities() {
    let mut capabilities = ServerCapabilities::default();

    assert!(!LspServerProcess::server_supports_text_document_open_close(
        &capabilities
    ));
    assert_eq!(
        LspServerProcess::server_text_document_sync_kind(&capabilities),
        None
    );
    assert!(!LspServerProcess::server_supports_text_document_save(
        &capabilities
    ));

    capabilities.text_document_sync =
        Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL));
    assert!(LspServerProcess::server_supports_text_document_open_close(
        &capabilities
    ));
    assert_eq!(
        LspServerProcess::server_text_document_sync_kind(&capabilities),
        Some(TextDocumentSyncKind::FULL)
    );
    assert!(!LspServerProcess::server_supports_text_document_save(
        &capabilities
    ));

    capabilities.text_document_sync = Some(TextDocumentSyncCapability::Options(
        TextDocumentSyncOptions {
            open_close: Some(true),
            change: Some(TextDocumentSyncKind::FULL),
            save: Some(TextDocumentSyncSaveOptions::Supported(true)),
            ..TextDocumentSyncOptions::default()
        },
    ));
    assert!(LspServerProcess::server_supports_text_document_open_close(
        &capabilities
    ));
    assert_eq!(
        LspServerProcess::server_text_document_sync_kind(&capabilities),
        Some(TextDocumentSyncKind::FULL)
    );
    assert!(LspServerProcess::server_supports_text_document_save(
        &capabilities
    ));

    capabilities.text_document_sync = Some(TextDocumentSyncCapability::Options(
        TextDocumentSyncOptions {
            change: Some(TextDocumentSyncKind::INCREMENTAL),
            ..TextDocumentSyncOptions::default()
        },
    ));
    assert_eq!(
        LspServerProcess::server_text_document_sync_kind(&capabilities),
        Some(TextDocumentSyncKind::INCREMENTAL)
    );

    capabilities.text_document_sync = Some(TextDocumentSyncCapability::Options(
        TextDocumentSyncOptions::default(),
    ));
    assert_eq!(
        LspServerProcess::server_text_document_sync_kind(&capabilities),
        None
    );
}

#[test]
fn full_document_change_is_serialized_without_a_range() -> anyhow::Result<()> {
    let (mut process, mut child, _app_receiver, _sender, _receiver, _tempdir, messages_path) =
        process_for_server_requests()?;
    let file_path: AbsolutePath = std::env::current_dir()?.join("main.ts").try_into()?;
    process.server_capabilities = Some(sync_capabilities(TextDocumentSyncKind::FULL));

    process.text_document_did_open(
        file_path.clone(),
        "typescript".to_string(),
        1,
        "const old = 1;".to_string(),
    )?;
    process.text_document_did_change(file_path, 2, "const updated = 2;".to_string())?;
    drop(process);
    child.wait()?;

    let messages = read_json_rpc_messages(&messages_path)?;
    let change = messages
        .iter()
        .find(|message| message["method"] == "textDocument/didChange")
        .ok_or_else(|| anyhow::anyhow!("missing didChange notification"))?;
    let event = &change["params"]["contentChanges"][0];
    assert_eq!(change["params"]["textDocument"]["version"], 2);
    assert_eq!(event["text"], "const updated = 2;");
    assert!(event.get("range").is_none_or(serde_json::Value::is_null));
    Ok(())
}

#[test]
fn incremental_changes_replace_the_previous_document_range() -> anyhow::Result<()> {
    let (mut process, mut child, _app_receiver, _sender, _receiver, _tempdir, messages_path) =
        process_for_server_requests()?;
    let file_path: AbsolutePath = std::env::current_dir()?.join("main.vue").try_into()?;
    process.server_capabilities = Some(sync_capabilities(TextDocumentSyncKind::INCREMENTAL));

    process.text_document_did_open(
        file_path.clone(),
        "vue".to_string(),
        1,
        "a😀\r\nb".to_string(),
    )?;
    process.text_document_did_change(file_path.clone(), 2, "x\n".to_string())?;
    process.text_document_did_change(file_path.clone(), 3, "final".to_string())?;
    process.text_document_did_close(file_path.clone())?;
    assert!(!process.synchronized_documents.contains_key(&file_path));
    drop(process);
    child.wait()?;

    let messages = read_json_rpc_messages(&messages_path)?;
    let changes = messages
        .iter()
        .filter(|message| message["method"] == "textDocument/didChange")
        .collect_vec();
    assert_eq!(changes.len(), 2);
    assert_eq!(
        changes[0]["params"]["contentChanges"][0],
        serde_json::json!({
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 1, "character": 1 }
            },
            "text": "x\n"
        })
    );
    assert_eq!(
        changes[1]["params"]["contentChanges"][0],
        serde_json::json!({
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 1, "character": 0 }
            },
            "text": "final"
        })
    );
    Ok(())
}

#[test]
fn incremental_change_requires_an_open_document_baseline() -> anyhow::Result<()> {
    let (mut process, mut child, _app_receiver, _sender, _receiver, _tempdir, messages_path) =
        process_for_server_requests()?;
    let file_path: AbsolutePath = std::env::current_dir()?.join("main.vue").try_into()?;
    process.server_capabilities = Some(sync_capabilities(TextDocumentSyncKind::INCREMENTAL));

    let error = process
        .text_document_did_change(file_path, 2, "content".to_string())
        .unwrap_err();
    assert!(error.to_string().contains("before textDocument/didOpen"));
    drop(process);
    child.wait()?;

    assert!(read_json_rpc_messages(&messages_path)?
        .iter()
        .all(|message| message["method"] != "textDocument/didChange"));
    Ok(())
}

#[test]
fn document_end_position_handles_encodings_and_line_endings() -> anyhow::Result<()> {
    assert_eq!(
        LspServerProcess::document_end_position("", &PositionEncodingKind::UTF16)?,
        Position::new(0, 0)
    );
    assert_eq!(
        LspServerProcess::document_end_position("a😀", &PositionEncodingKind::UTF8)?,
        Position::new(0, 5)
    );
    assert_eq!(
        LspServerProcess::document_end_position("a😀", &PositionEncodingKind::UTF16)?,
        Position::new(0, 3)
    );
    assert_eq!(
        LspServerProcess::document_end_position("a😀", &PositionEncodingKind::UTF32)?,
        Position::new(0, 2)
    );
    assert_eq!(
        LspServerProcess::document_end_position("a\n", &PositionEncodingKind::UTF16)?,
        Position::new(1, 0)
    );
    assert_eq!(
        LspServerProcess::document_end_position("a\r\nb\rc\n", &PositionEncodingKind::UTF16,)?,
        Position::new(3, 0)
    );
    Ok(())
}

#[test]
fn text_document_did_close_is_serialized_when_supported() -> anyhow::Result<()> {
    let (mut process, mut child, _app_receiver, _sender, _receiver, _tempdir, messages_path) =
        process_for_server_requests()?;
    let file_path: AbsolutePath = std::env::current_dir()?.join("Main.java").try_into()?;

    process.text_document_did_close(file_path.clone())?;
    process.server_capabilities = Some(ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                ..TextDocumentSyncOptions::default()
            },
        )),
        ..ServerCapabilities::default()
    });
    process.text_document_did_close(file_path.clone())?;
    drop(process);
    child.wait()?;

    let messages = std::fs::read_to_string(messages_path)?;
    assert_eq!(messages.matches("textDocument/didClose").count(), 1);
    assert!(messages.contains(&file_path.display_absolute()));
    Ok(())
}
