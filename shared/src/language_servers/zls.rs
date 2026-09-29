use crate::language::{Command, LspCommand};

pub fn command() -> LspCommand {
    LspCommand {
        command: Command::new("zls", &[]),
        ..LspCommand::default()
    }
}
