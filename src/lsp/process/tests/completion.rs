use super::super::*;
use super::support::{process_for_server_requests, read_json_rpc_messages};
use crate::{
    lsp::completion::session::{CompletionItemSource, CompletionRequest},
    position::Position,
};

#[test]
fn empty_capability_registration_batches_are_successful_noops() -> anyhow::Result<()> {
    let (mut process, mut child, _app_receiver, _sender, _receiver, _tempdir, messages_path) =
        process_for_server_requests()?;
    for (id, (method, field)) in [
        ("client/registerCapability", "registrations"),
        ("client/unregisterCapability", "unregisterations"),
    ]
    .into_iter()
    .enumerate()
    {
        process.handle_reply(serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": { field: [] }
        }))?;
    }
    drop(process);
    child.wait()?;
    let messages = read_json_rpc_messages(&messages_path)?;
    assert_eq!(messages.len(), 2);
    assert!(messages.iter().all(
        |message| message.get("result") == Some(&serde_json::Value::Null)
            && message.get("error").is_none()
    ));
    Ok(())
}

#[test]
fn completion_protocol_preserves_request_identity_and_empty_responses() -> anyhow::Result<()> {
    let (mut process, mut child, app_receiver, _sender, _receiver, _tempdir, messages_path) =
        process_for_server_requests()?;
    process.language = crate::config::from_extension("vue").unwrap();
    process.server_capabilities = Some(ServerCapabilities {
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![":".to_string()]),
            ..Default::default()
        }),
        ..Default::default()
    });
    let path: AbsolutePath = std::env::current_dir()?.join("Component.vue").try_into()?;
    let request = CompletionRequest {
        generation: 7,
        path: path.clone(),
        version: 3,
        position: Position::new(0, 20),
    };
    let params = RequestParams {
        path,
        position: request.position,
        selection_end: request.position,
        context: ResponseContext {
            completion: Some(Arc::new(CompletionContext::Request(request.clone()))),
            ..Default::default()
        },
    };
    for (id, result) in [serde_json::Value::Null, serde_json::json!({ "isIncomplete": true, "items": [{ "label": "flex", "data": { "_projectKey": "ui" } }] })].into_iter().enumerate() {
        process.text_document_completion(params.clone())?;
        process.handle_reply(serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }))?;
        let AppMessage::LspNotification(notification) = app_receiver.recv()? else { panic!("expected notification") };
        let LspNotification::Completion(response) = *notification else { panic!("expected completion") };
        assert_eq!(response.request, request);
        assert_eq!(response.server.server_id, "test");
        assert_eq!(response.trigger_characters, [":"]);
        assert_eq!(response.items.len(), id);
        if let Some(item) = response.items.first() {
            assert_eq!(item.data.as_ref().unwrap()["_projectKey"], "ui");
        }
    }
    drop(process);
    child.wait()?;
    let messages = read_json_rpc_messages(&messages_path)?;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["method"], "textDocument/completion");
    Ok(())
}

#[test]
fn completion_resolve_keeps_opaque_server_data_and_local_owner_separate() -> anyhow::Result<()> {
    let (mut process, mut child, app_receiver, _sender, _receiver, _tempdir, messages_path) =
        process_for_server_requests()?;
    process.server_capabilities = Some(ServerCapabilities {
        completion_provider: Some(CompletionOptions {
            resolve_provider: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    });
    let path: AbsolutePath = std::env::current_dir()?.join("Component.vue").try_into()?;
    let source = CompletionItemSource {
        request: CompletionRequest {
            generation: 7,
            path: path.clone(),
            version: 3,
            position: Position::new(0, 20),
        },
        server: LspServerKey {
            language_id: "vue".to_string(),
            server_id: "tailwindcss".to_string(),
            root: process.current_working_directory.clone(),
        },
        index: 4,
    };
    let item = lsp_types::CompletionItem {
        label: "flex".to_string(),
        data: Some(serde_json::json!({"_projectKey":"ui"})),
        ..Default::default()
    };
    let context = ResponseContext {
        completion: Some(Arc::new(CompletionContext::Resolve(source))),
        ..Default::default()
    };
    process.completion_item_resolve(
        RequestParams {
            path,
            position: Position::default(),
            selection_end: Position::default(),
            context: context.clone(),
        },
        item.clone(),
    )?;
    process.handle_reply(serde_json::json!({ "jsonrpc": "2.0", "id": 0, "result": { "label": "flex", "documentation": "display: flex", "data": item.data } }))?;
    let AppMessage::LspNotification(notification) = app_receiver.recv()? else {
        panic!("expected notification")
    };
    let LspNotification::CompletionItemResolve(actual_context, resolved) = *notification else {
        panic!("expected resolved completion")
    };
    assert_eq!(actual_context, context);
    assert!(resolved.documentation.is_some());
    drop(process);
    child.wait()?;
    let messages = read_json_rpc_messages(&messages_path)?;
    assert_eq!(messages[0]["method"], "completionItem/resolve");
    assert_eq!(messages[0]["params"], serde_json::to_value(item)?);
    Ok(())
}
