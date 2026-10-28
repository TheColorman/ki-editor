use super::super::*;
use super::support::process_for_server_requests;

#[test]
fn omnisharp_project_problems_are_displayed_as_warnings() -> anyhow::Result<()> {
    let (mut process, mut child, app_receiver, _, _, _tempdir, _) = process_for_server_requests()?;
    // A wrapper retains the built-in behavior through its configured server ID.
    process.server_config = LspServerConfig::new(
        "omnisharp",
        shared::language::Command::new("custom-wrapper", &[]),
    );
    process.handle_reply(serde_json::json!({
        "jsonrpc": "2.0",
        "method": "o#/msbuildprojectdiagnostics",
        "params": {
            "FileName": "/project/App.csproj",
            "Errors": [{ "Text": "Warning As Error: Package has a known vulnerability" }],
            "Warnings": []
        }
    }))?;
    let AppMessage::LspNotification(notification) = app_receiver.try_recv()? else {
        panic!("expected an LSP notification");
    };
    let LspNotification::ShowMessage {
        server_id,
        typ,
        message,
    } = *notification
    else {
        panic!("expected a visible project warning");
    };
    assert_eq!(server_id, "omnisharp");
    assert_eq!(typ, MessageType::WARNING);
    assert!(message.contains("/project/App.csproj"));
    assert!(message.contains("Package has a known vulnerability"));
    process.handle_reply(serde_json::json!({
        "jsonrpc": "2.0",
        "method": "o#/msbuildprojectdiagnostics",
        "params": { "FileName": "/project/App.csproj", "Errors": [], "Warnings": [] }
    }))?;
    assert!(app_receiver.try_recv().is_err());
    process.server_config =
        LspServerConfig::new("other", shared::language::Command::new("other-server", &[]));
    process.handle_reply(serde_json::json!({
        "jsonrpc": "2.0",
        "method": "o#/msbuildprojectdiagnostics",
        "params": {
            "FileName": "/project/App.csproj",
            "Errors": [{ "Text": "not an OmniSharp server" }]
        }
    }))?;
    assert!(app_receiver.try_recv().is_err());
    drop(process);
    child.wait()?;
    Ok(())
}

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
        "params": { "registrations": [{ "id": "hover", "method": "textDocument/hover" }] }
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
