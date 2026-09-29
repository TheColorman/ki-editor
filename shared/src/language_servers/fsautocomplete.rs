use crate::language::{Command, LspCommand};

pub fn command() -> LspCommand {
    LspCommand {
        // Use --log-file and --log-level arguments to debug fsautocomplete issues.
        // Example: --log-file /path/to/fsac.log --log-level debug
        command: Command::new("fsautocomplete", &[]),
        ..LspCommand::default()
    }
}
