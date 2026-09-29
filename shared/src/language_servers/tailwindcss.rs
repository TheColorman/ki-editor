use crate::language::{Command, LanguageId, LspServerConfig};

pub fn config(language_id: &'static str) -> LspServerConfig {
    LspServerConfig {
        language_id: Some(LanguageId::new(language_id)),
        primary: false,
        completion: Some(true),
        ..LspServerConfig::new(
            "tailwindcss",
            Command::new("tailwindcss-language-server", &["--stdio"]),
        )
    }
}
