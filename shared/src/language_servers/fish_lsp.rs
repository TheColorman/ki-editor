use crate::language::{Command, LspCommand};

pub fn command() -> LspCommand {
    LspCommand {
        command: Command::new("fish-lsp", &["start"]),
        ..LspCommand::default()
    }
}
