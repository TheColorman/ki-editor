use crate::language::{Command, LspCommand};

pub fn command() -> LspCommand {
    LspCommand {
        command: Command::new("emmet-language-server", &["--stdio"]),
        ..LspCommand::default()
    }
}
