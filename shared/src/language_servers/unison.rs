use crate::language::{Command, LspCommand};

pub fn command() -> LspCommand {
    LspCommand {
        command: Command::new("nc", &["localhost", "5757"]),
        ..LspCommand::default()
    }
}
