use super::super::*;
use super::support::process_for_server_requests;

#[test]
fn show_message_request_is_displayed_and_cancelled() -> anyhow::Result<()> {
    let (mut process, mut child, app_receiver, _sender, _receiver, _tempdir, messages_path) =
        process_for_server_requests()?;

    process.handle_reply(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "window/showMessageRequest",
        "params": {
            "type": 2,
            "message": "Update the Java project configuration?",
            "actions": [{ "title": "Update" }]
        }
    }))?;

    let AppMessage::LspNotification(notification) = app_receiver.recv()? else {
        panic!("expected an LSP notification");
    };
    assert!(matches!(
        *notification,
        LspNotification::ShowMessage {
            typ: MessageType::WARNING,
            ..
        }
    ));
    drop(process);
    child.wait()?;
    let messages = std::fs::read_to_string(messages_path)?;
    assert!(messages.contains("\"id\":7"));
    assert!(messages.contains("\"result\":null"));
    Ok(())
}

#[test]
fn dynamic_capability_registration_is_rejected() -> anyhow::Result<()> {
    let (mut process, mut child, _app_receiver, _sender, _receiver, _tempdir, messages_path) =
        process_for_server_requests()?;

    process.handle_reply(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 9,
        "method": "client/registerCapability",
        "params": { "registrations": [] }
    }))?;

    drop(process);
    child.wait()?;
    let messages = std::fs::read_to_string(messages_path)?;
    assert!(messages.contains("\"id\":9"));
    assert!(messages.contains("\"code\":-32601"));
    Ok(())
}

#[test]
fn workspace_edit_reports_application_failure() -> anyhow::Result<()> {
    let (mut process, mut child, app_receiver, sender, receiver, _tempdir, messages_path) =
        process_for_server_requests()?;

    process.handle_reply(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 11,
        "method": "workspace/applyEdit",
        "params": { "edit": { "changes": {} } }
    }))?;
    let AppMessage::LspNotification(notification) = app_receiver.recv()? else {
        panic!("expected an LSP notification");
    };
    let LspNotification::WorkspaceEdit {
        respond: Some(respond),
        ..
    } = *notification
    else {
        panic!("expected a workspace edit request");
    };
    respond.call(Err("edit failed".to_string()));
    sender.send(LspServerProcessMessage::Exit)?;
    process.process_messages(receiver);
    drop(process);
    child.wait()?;

    let messages = std::fs::read_to_string(messages_path)?;
    assert!(messages.contains("\"id\":11"));
    assert!(messages.contains("\"applied\":false"));
    assert!(messages.contains("edit failed"));
    Ok(())
}
