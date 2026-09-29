use super::super::*;

#[test]
fn workspace_configuration_uses_the_requesting_servers_settings() {
    let root: AbsolutePath = "/tmp/project".try_into().unwrap();
    let server = serde_json::from_value(serde_json::json!({
        "id": "test",
        "command": { "command": "test-server", "arguments": [] },
        "initialization_options": {
            "settings": {
                "editor": { "enabled": false, "sdk": "${workspace}/sdk" }
            }
        }
    }))
    .unwrap();
    let response = workspace_configuration_response(
        ConfigurationParams {
            items: [
                Some("editor"),
                Some("editor.enabled"),
                Some("unknown"),
                None,
            ]
            .into_iter()
            .map(|section| ConfigurationItem {
                scope_uri: None,
                section: section.map(str::to_string),
            })
            .collect(),
        },
        &root,
        &server,
    );
    assert_eq!(
        response[0],
        serde_json::json!({"enabled": false, "sdk": "/tmp/project/sdk"})
    );
    assert_eq!(response[1], false);
    assert_eq!(response[2], serde_json::Value::Null);
    assert_eq!(response[3]["editor"], response[0]);
}

#[test]
fn unknown_server_has_no_implicit_settings() {
    let root: AbsolutePath = "/tmp/project".try_into().unwrap();
    let server = LspServerConfig::new("test", shared::language::Command::new("test-server", &[]));
    let response = workspace_configuration_response(
        ConfigurationParams {
            items: vec![ConfigurationItem {
                scope_uri: None,
                section: Some("".to_string()),
            }],
        },
        &root,
        &server,
    );
    assert_eq!(response, serde_json::json!([null]));
}

#[test]
fn generic_error_hints_do_not_require_a_builtin_server() {
    let server = LspServerConfig::new("test", shared::language::Command::new("test-server", &[]));
    assert!(hint_for_lsp_error(&server, "Cannot find module example").is_some());
    assert!(hint_for_lsp_error(&server, "Unexpected response").is_none());
}
