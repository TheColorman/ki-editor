//! Built-in language server definitions, grouped by server rather than language.
//!
//! Languages select servers here; each server owns its launch command and any
//! server-specific initialization, configuration, or discovery helpers.
//!
//! Servers declare workspace markers in a `workspace_root::WorkspaceRootPolicy`.
//! Additional runtime customization implements `definition::ServerBehavior`.
//! Both are registered through a `ServerDefinition` below, keeping the common
//! LSP code independent of individual servers.

pub mod bash_language_server;
pub mod cl_lsp;
pub mod clangd;
pub mod configuration;
pub mod definition;
pub mod elixir_ls;
pub mod emmet_language_server;
pub mod eslint;
pub mod fish_lsp;
pub mod fsautocomplete;
pub mod gleam;
pub mod glsl_analyzer;
pub mod gopls;
pub mod graphql_lsp;
pub mod haskell_language_server;
pub mod idris2_lsp;
pub mod jdtls;
pub mod julia;
pub mod lua_language_server;
pub mod marksman;
pub mod metals;
pub mod nil;
pub mod ocamllsp;
pub mod ols;
pub mod omnisharp;
mod package_workspace;
pub mod perlnavigator;
pub mod pyright;
pub mod qmlls;
pub mod racket;
pub mod rescript_language_server;
pub mod ruby_lsp;
pub mod rust_analyzer;
pub mod sourcekit_lsp;
pub mod svelte;
pub mod texlab;
pub mod tinymist;
pub mod typescript_language_server;
pub mod unison;
pub mod vtsls;
pub mod workspace_root;
pub mod zls;

const GENERIC_DEFINITION: definition::ServerDefinition = definition::ServerDefinition {
    ids: &[],
    executables: &[],
    behavior: &definition::GenericServer,
    root_policy: &package_workspace::ROOT_POLICY,
};

/// Registration is the only place that enumerates specialized server policies.
/// Servers with ordinary launch commands use the generic implementation.
pub fn definition_for(
    config: &crate::language::LspServerConfig,
) -> &'static definition::ServerDefinition {
    [
        &vtsls::DEFINITION,
        &eslint::DEFINITION,
        &jdtls::DEFINITION,
        &omnisharp::DEFINITION,
        &typescript_language_server::DEFINITION,
    ]
    .into_iter()
    .find(|definition| definition.matches(config))
    .unwrap_or(&GENERIC_DEFINITION)
}
