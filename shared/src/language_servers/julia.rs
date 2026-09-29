//! Launch definition retained for Julia's currently disabled default server.

use crate::language::{Command, LspCommand};

pub fn command() -> LspCommand {
    LspCommand {
        command: Command::new(
            "julia",
            &[
                "--startup-file=no",
                "--history-file=no",
                "--quiet",
                "-e",
                "'using LanguageServer; runserver()'",
            ],
        ),
        ..LspCommand::default()
    }
}
