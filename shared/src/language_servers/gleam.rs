use crate::language::{Command, LspCommand};

pub fn command() -> LspCommand {
    LspCommand {
        command: Command::new("gleam", &["lsp"]),
        ..LspCommand::default()
    }
}
