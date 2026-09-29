use crate::language::{Command, LspCommand};

pub fn command() -> LspCommand {
    LspCommand {
        command: Command::new("idris2-lsp", &[]),
        ..LspCommand::default()
    }
}
