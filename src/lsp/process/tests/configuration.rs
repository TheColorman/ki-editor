use super::super::*;

#[test]
fn workspace_configuration_includes_vue_eslint_validation() {
    let root: AbsolutePath = std::env::current_dir().unwrap().try_into().unwrap();
    let response = workspace_configuration_response(
        ConfigurationParams {
            items: vec![ConfigurationItem {
                scope_uri: None,
                section: Some("eslint".to_string()),
            }],
        },
        &root,
        None,
    );
    let configs = response.as_array().unwrap();
    assert!(configs[0]["validate"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("vue")));
}

#[test]
fn workspace_configuration_includes_vue_typescript_plugin_for_vtsls() {
    let root: AbsolutePath = std::env::current_dir().unwrap().try_into().unwrap();
    let response = workspace_configuration_response(
        ConfigurationParams {
            items: vec![ConfigurationItem {
                scope_uri: None,
                section: Some("".to_string()),
            }],
        },
        &root,
        None,
    );
    let configs = response.as_array().unwrap();
    assert_eq!(
        configs[0]["typescript.tsdk"],
        format!(
            "{}/node_modules/typescript/lib",
            std::env::current_dir().unwrap().display()
        )
    );
    assert_eq!(
        configs[0]["vtsls"]["tsserver"]["globalPlugins"][0]["name"],
        "@vue/typescript-plugin"
    );
}

#[test]
fn workspace_configuration_uses_java_initialization_settings() {
    let root: AbsolutePath = "/tmp/java-project".try_into().unwrap();
    let initialization_options = serde_json::json!({
        "settings": {
            "java": {
                "signatureHelp": { "enabled": true },
                "configuration": {
                    "runtimes": [{ "path": "${workspace}/jdk", "name": "JavaSE-21" }]
                }
            }
        }
    });
    let response = workspace_configuration_response(
        ConfigurationParams {
            items: vec![
                ConfigurationItem {
                    scope_uri: None,
                    section: Some("java".to_string()),
                },
                ConfigurationItem {
                    scope_uri: None,
                    section: Some("java.signatureHelp.enabled".to_string()),
                },
                ConfigurationItem {
                    scope_uri: None,
                    section: Some("unknown".to_string()),
                },
            ],
        },
        &root,
        Some(&initialization_options),
    );
    let configs = response.as_array().unwrap();

    assert_eq!(configs[0]["signatureHelp"]["enabled"], true);
    assert_eq!(
        configs[0]["configuration"]["runtimes"][0]["path"],
        "/tmp/java-project/jdk"
    );
    assert_eq!(configs[1], true);
    assert_eq!(configs[2], serde_json::Value::Null);
}

#[test]
fn lsp_error_hint_mentions_vue_typescript_plugin() {
    let hint = hint_for_lsp_error(
        "Request textDocument/definition failed with message: Cannot find provider for definition, the feature is possibly not supported by the current TypeScript version or disabled by settings.",
    )
    .unwrap();

    assert!(hint.contains("@vue/typescript-plugin"));
}

#[test]
fn lsp_error_hint_mentions_eslint_resolution() {
    let hint = hint_for_lsp_error(
        "Request textDocument/diagnostic failed with message: The \"path\" argument must be of type string. Received undefined",
    )
    .unwrap();

    assert!(hint.contains("ESLint"));
}
