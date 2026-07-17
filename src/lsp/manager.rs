use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use crate::app::AppMessage;
use crate::lsp::server_config::resolve_configured_path;

use super::process::{FromEditor, LspNotification, LspServerProcessChannel};
use shared::{absolute_path::AbsolutePath, language::Language};

fn is_package_workspace_root(path: &Path) -> bool {
    [
        "pnpm-workspace.yaml",
        "pnpm-workspace.yml",
        "pnpm-lock.yaml",
        "yarn.lock",
        "package-lock.json",
        "bun.lockb",
        "bun.lock",
    ]
    .into_iter()
    .any(|marker| path.join(marker).exists())
}

fn contains_file_with_extension(path: &Path, extensions: &[&str]) -> bool {
    std::fs::read_dir(path).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    extensions
                        .iter()
                        .any(|candidate| extension.eq_ignore_ascii_case(candidate))
                })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsp_root_prefers_nested_package_workspace_root() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let root: AbsolutePath = tempdir.path().try_into()?;
        let frontend = tempdir.path().join("frontends");
        let app = frontend.join("apps/launch/app");
        std::fs::create_dir_all(&app)?;
        std::fs::write(frontend.join("pnpm-workspace.yaml"), "packages: []")?;
        std::fs::write(app.join("app.vue"), "<template />")?;

        let (sender, _receiver) = crossbeam_channel::unbounded();
        let manager = LspManager::new(sender, root);
        let language = crate::config::from_extension("vue").unwrap();
        let actual = manager.lsp_root_for_path(&language, &app.join("app.vue").try_into()?);

        assert_eq!(actual.as_ref(), frontend.as_path());
        Ok(())
    }

    #[test]
    fn lsp_root_treats_pnpm_lockfile_as_package_root() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let root: AbsolutePath = tempdir.path().try_into()?;
        let frontend = tempdir.path().join("frontends");
        let app = frontend.join("apps/launch/app");
        std::fs::create_dir_all(&app)?;
        std::fs::write(frontend.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'")?;
        std::fs::write(app.join("app.vue"), "<template />")?;

        let (sender, _receiver) = crossbeam_channel::unbounded();
        let manager = LspManager::new(sender, root);
        let language = crate::config::from_extension("vue").unwrap();
        let actual = manager.lsp_root_for_path(&language, &app.join("app.vue").try_into()?);

        assert_eq!(actual.as_ref(), frontend.as_path());
        Ok(())
    }

    #[test]
    fn csharp_lsp_root_prefers_solution_over_project() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let root: AbsolutePath = tempdir.path().try_into()?;
        let solution = tempdir.path().join("services/api");
        let project = solution.join("src/App");
        std::fs::create_dir_all(&project)?;
        std::fs::write(solution.join("App.sln"), "")?;
        std::fs::write(project.join("App.csproj"), "")?;
        std::fs::write(project.join("Program.cs"), "")?;

        let (sender, _receiver) = crossbeam_channel::unbounded();
        let manager = LspManager::new(sender, root);
        let language = crate::config::from_extension("cs").unwrap();
        let actual = manager.lsp_root_for_path(&language, &project.join("Program.cs").try_into()?);

        assert_eq!(actual.as_ref(), solution);
        Ok(())
    }

    #[test]
    fn csharp_lsp_root_falls_back_to_nearest_project() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let root: AbsolutePath = tempdir.path().try_into()?;
        let project = tempdir.path().join("src/App");
        std::fs::create_dir_all(&project)?;
        std::fs::write(project.join("App.csproj"), "")?;
        std::fs::write(project.join("Program.cs"), "")?;

        let (sender, _receiver) = crossbeam_channel::unbounded();
        let manager = LspManager::new(sender, root);
        let language = crate::config::from_extension("cs").unwrap();
        let actual = manager.lsp_root_for_path(&language, &project.join("Program.cs").try_into()?);

        assert_eq!(actual.as_ref(), project);
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct LspServerKey {
    language_id: String,
    server_id: String,
    root: AbsolutePath,
}

pub struct LspManager {
    lsp_server_process_channels: HashMap<LspServerKey, LspServerProcessChannel>,
    sender: crossbeam_channel::Sender<AppMessage>,
    current_working_directory: AbsolutePath,
    #[cfg(test)]
    /// Used for testing the correctness of LSP requests
    /// We use HashMap instead of Vec because we only one to store the latest
    /// requests of the same kind
    history: HashMap</* request name */ &'static str, FromEditor>,

    #[cfg(test)]
    /// Used for testing the correctness of initialization
    lsp_server_initialized_args_history: Vec<(String, Vec<AbsolutePath>)>,
}

impl Drop for LspManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl LspManager {
    pub fn new(
        sender: crossbeam_channel::Sender<AppMessage>,
        current_working_directory: AbsolutePath,
    ) -> LspManager {
        LspManager {
            lsp_server_process_channels: HashMap::new(),
            sender,
            current_working_directory,
            #[cfg(test)]
            history: HashMap::default(),
            #[cfg(test)]
            lsp_server_initialized_args_history: Vec::default(),
        }
    }

    fn server_key(
        language: &Language,
        server_id: &str,
        root: AbsolutePath,
    ) -> Option<LspServerKey> {
        Some(LspServerKey {
            language_id: language.id()?.to_string(),
            server_id: server_id.to_string(),
            root,
        })
    }

    pub(crate) fn lsp_root_for_path(
        &self,
        language: &Language,
        path: &AbsolutePath,
    ) -> AbsolutePath {
        let file_parent = path.as_ref().parent().unwrap_or(path.as_ref());
        let boundary = self.current_working_directory.as_ref();
        let mut nearest_package_root = None::<PathBuf>;
        let mut nearest_csharp_project = None::<PathBuf>;
        let is_csharp = language.tree_sitter_grammar_id().as_deref() == Some("c_sharp");

        for ancestor in file_parent.ancestors() {
            if is_csharp {
                if contains_file_with_extension(ancestor, &["sln", "slnx"]) {
                    return ancestor
                        .try_into()
                        .unwrap_or_else(|_| self.current_working_directory.clone());
                }
                if nearest_csharp_project.is_none()
                    && contains_file_with_extension(ancestor, &["csproj"])
                {
                    nearest_csharp_project = Some(ancestor.to_path_buf());
                }
            }
            if is_package_workspace_root(ancestor) {
                if is_csharp {
                    nearest_package_root.get_or_insert_with(|| ancestor.to_path_buf());
                } else {
                    return ancestor
                        .try_into()
                        .unwrap_or_else(|_| self.current_working_directory.clone());
                }
            }
            if nearest_package_root.is_none() && ancestor.join("package.json").exists() {
                nearest_package_root = Some(ancestor.to_path_buf());
            }
            if ancestor == boundary {
                break;
            }
        }

        nearest_csharp_project
            .or(nearest_package_root)
            .and_then(|path| path.as_path().try_into().ok())
            .unwrap_or_else(|| self.current_working_directory.clone())
    }

    fn is_lifecycle_message(from_editor: &FromEditor) -> bool {
        matches!(
            from_editor,
            FromEditor::TextDocumentDidOpen { .. }
                | FromEditor::TextDocumentDidChange { .. }
                | FromEditor::TextDocumentDidSave { .. }
                | FromEditor::WorkspaceDidRenameFiles { .. }
                | FromEditor::WorkspaceDidCreateFiles { .. }
        )
    }

    fn warn_missing_configured_lsp_paths(
        &self,
        root: &AbsolutePath,
        server_id: &str,
        config: &shared::language::LspServerConfig,
    ) {
        let Some(options) = config.initialization_options() else {
            return;
        };

        if let Some(tsdk) = options
            .pointer("/typescript/tsdk")
            .or_else(|| options.pointer("/typescript.tsdk"))
            .and_then(|value| value.as_str())
        {
            let Some(resolved) = resolve_configured_path(root, tsdk) else {
                log::warn!(
                    "Unable to resolve configured TypeScript SDK placeholder {tsdk:?} for LSP server '{server_id}'"
                );
                return;
            };
            if !resolved.exists() {
                log::warn!(
                    "Configured LSP path for server '{server_id}' does not exist: typescript tsdk {tsdk:?} resolved to {}. If the server is installed/configured through Nix or another package manager, override this path in Ki config.",
                    resolved.display()
                );
            }
        }

        if let Some(plugins) = options
            .pointer("/vtsls/tsserver/globalPlugins")
            .and_then(|value| value.as_array())
        {
            for plugin in plugins {
                let Some(location) = plugin.get("location").and_then(|value| value.as_str()) else {
                    continue;
                };
                let Some(resolved) = resolve_configured_path(root, location) else {
                    let name = plugin
                        .get("name")
                        .and_then(|value| value.as_str())
                        .unwrap_or("<unknown plugin>");
                    log::warn!(
                        "Unable to resolve configured LSP plugin placeholder for server '{server_id}': plugin {name:?} location {location:?}. If the plugin is provided by Nix or another package manager, override this location in Ki config."
                    );
                    continue;
                };
                if !resolved.exists() {
                    let name = plugin
                        .get("name")
                        .and_then(|value| value.as_str())
                        .unwrap_or("<unknown plugin>");
                    log::warn!(
                        "Configured LSP plugin path for server '{server_id}' does not exist: plugin {name:?} location {location:?} resolved to {}. If the plugin is provided by Nix or another package manager, override this location in Ki config.",
                        resolved.display()
                    );
                }
            }
        }
    }

    fn invoke_channels(
        &self,
        path: &AbsolutePath,
        from_editor: &FromEditor,
        f: impl Fn(&LspServerProcessChannel) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let Some(language) = crate::config::from_path(path) else {
            return Ok(());
        };
        let configs = language.lsp_server_configs();
        let root = self.lsp_root_for_path(&language, path);
        let configs = if Self::is_lifecycle_message(from_editor) {
            configs
        } else {
            configs
                .into_iter()
                .filter(|config| config.primary())
                .collect()
        };

        for config in configs {
            if let Some(channel) = Self::server_key(&language, config.id(), root.clone())
                .and_then(|key| self.lsp_server_process_channels.get(&key))
            {
                f(channel)?;
            }
        }
        Ok(())
    }

    pub fn send_message(
        &mut self,
        path: AbsolutePath,
        from_editor: FromEditor,
    ) -> anyhow::Result<()> {
        #[cfg(test)]
        self.history
            .insert(from_editor.variant(), from_editor.clone());

        self.invoke_channels(&path, &from_editor, |channel| {
            channel.send_from_editor(from_editor.clone())
        })
    }

    /// Open file can do one of the following:
    /// 1. Start a new LSP server process if it is not started yet.
    /// 2. Notify the LSP server process that a new file is opened.
    /// 3. Do nothing if the LSP server process is spawned but not yet initialized.
    pub fn open_file(&mut self, path: AbsolutePath) -> Result<(), anyhow::Error> {
        let Some(language) = crate::config::from_path(&path) else {
            return Ok(());
        };

        for config in language.lsp_server_configs() {
            let lsp_root = self.lsp_root_for_path(&language, &path);
            let Some(server_key) = Self::server_key(&language, config.id(), lsp_root.clone())
            else {
                continue;
            };
            if let Some(channel) = self.lsp_server_process_channels.get(&server_key) {
                if channel.is_initialized() {
                    channel.document_did_open(path.clone())?;
                }
            } else {
                let is_primary = config.primary();
                let server_id = config.id().to_string();
                let command = config.process_command().to_string();
                self.warn_missing_configured_lsp_paths(&lsp_root, &server_id, &config);
                match LspServerProcessChannel::new(
                    language.clone(),
                    config,
                    self.sender.clone(),
                    lsp_root.clone(),
                ) {
                    Ok(Some(channel)) => {
                        log::info!(
                            "Started LSP server '{server_id}' ({command}) for {path:?} with root {lsp_root:?}"
                        );
                        self.lsp_server_process_channels.insert(server_key, channel);
                    }
                    Ok(None) => {}
                    Err(error) if is_primary => {
                        log::error!(
                            "Failed to start primary LSP server '{server_id}' ({command}) for {path:?}: {error:?}"
                        );
                        return Err(error);
                    }
                    Err(error) => {
                        log::warn!(
                            "Failed to start secondary LSP server '{server_id}' ({command}) for {path:?}: {error:?}"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    pub fn initialized(
        &mut self,
        language: Language,
        server_id: String,
        root: AbsolutePath,
        opened_documents: Vec<AbsolutePath>,
    ) {
        let Some(server_key) = Self::server_key(&language, &server_id, root) else {
            return;
        };

        #[cfg(test)]
        self.lsp_server_initialized_args_history.push((
            format!("{}:{}", server_key.language_id, server_key.server_id),
            opened_documents.clone(),
        ));

        self.lsp_server_process_channels
            .get_mut(&server_key)
            .map(|channel| {
                channel.initialized();
                channel.documents_did_open(opened_documents)
            });
    }

    pub fn shutdown(&mut self) {
        for (_, channel) in self.lsp_server_process_channels.drain() {
            channel
                .shutdown()
                .unwrap_or_else(|error| log::error!("{error:?}"));
        }
    }

    /// Restarts the LSP server process for the given `language`, if one is running.
    ///
    /// The existing process (if any) is shut down and a fresh one is spawned.
    /// Once the new process reports that it is initialized, `documents_did_open`
    /// will be replayed for currently open buffers (see `App::handle_lsp_notification`),
    /// so callers do not need to re-open any documents themselves.
    pub fn restart_language(&mut self, language: &Language) -> anyhow::Result<()> {
        let Some(language_id) = language.id() else {
            return Ok(());
        };

        if let Some(channel) = self.lsp_server_process_channels.remove(&language_id) {
            // `shutdown` blocks until the old process has actually stopped (or failed
            // to), so a failure is reported here before the replacement process is
            // spawned below, rather than racing with it. Success is not reported —
            // it's the expected outcome and not worth surfacing to the user.
            if let Err(error) = channel.shutdown() {
                let _ = self.sender.send(AppMessage::LspNotification(Box::new(
                    LspNotification::Error(format!(
                        "LSP server for {language_id} failed to shut down cleanly: {error:?}"
                    )),
                )));
            }
        }

        LspServerProcessChannel::new(
            language.clone(),
            self.sender.clone(),
            self.current_working_directory.clone(),
        )
        .map(|channel| {
            if let Some(channel) = channel {
                self.lsp_server_process_channels
                    .insert(language_id, channel);
            }
        })
    }

    #[cfg(test)]
    pub fn lsp_request_sent(&self, from_editor: &FromEditor) -> bool {
        self.history.get(from_editor.variant()) == Some(from_editor)
    }

    #[cfg(test)]
    pub fn lsp_server_initialized_args(&self) -> Option<(String, Vec<AbsolutePath>)> {
        self.lsp_server_initialized_args_history.last().cloned()
    }
}
