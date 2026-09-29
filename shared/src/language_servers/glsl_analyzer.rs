use crate::language::{Command, LspCommand};

pub fn command() -> LspCommand {
    LspCommand {
        command: Command::new("glsl_analyzer", &[]),
        ..LspCommand::default()
    }
}
