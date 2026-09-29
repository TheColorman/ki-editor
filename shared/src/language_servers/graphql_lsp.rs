use crate::language::{Command, LspCommand};

pub fn command() -> LspCommand {
    LspCommand {
        command: Command::new("graphql-lsp", &["server", "-m", "stream"]),
        initialization_options: Some(
            serde_json::json! {r#"{ "graphql-config.load.legacy": true }"#.to_string()},
        ),
        ..LspCommand::default()
    }
}
