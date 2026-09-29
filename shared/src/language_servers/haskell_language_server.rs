use crate::language::{Command, LspCommand};

pub fn command() -> LspCommand {
    LspCommand {
        command: Command::new("haskell-language-server-wrapper", &["--lsp"]),
        ..LspCommand::default()
    }
}
