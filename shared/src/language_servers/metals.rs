use crate::language::{Command, LspCommand};

pub fn command() -> LspCommand {
    LspCommand {
        command: Command::new("metals", &[]),
        ..LspCommand::default()
    }
}
