use crate::language::{Command, LspCommand};

pub fn command() -> LspCommand {
    LspCommand {
        command: Command::new("cl-lsp", &[]),
        ..LspCommand::default()
    }
}
