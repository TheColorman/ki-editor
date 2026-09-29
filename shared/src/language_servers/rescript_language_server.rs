use crate::language::{Command, LspCommand};

pub fn command() -> LspCommand {
    LspCommand {
        command: Command::new("./node_modules/.bin/rescript-language-server", &["--stdio"]),
        ..LspCommand::default()
    }
}
