use std::collections::HashMap;

use crate::app::AppMessage;
use shared::{absolute_path::AbsolutePath, language::Language};

use super::process::{FromEditor, LspNotification, LspServerProcessChannel};
use super::server_config::root_for_path;

pub struct LspManager {
    lsp_server_process_channels: HashMap<(String, AbsolutePath), LspServerProcessChannel>,
    sender: crossbeam_channel::Sender<AppMessage>,
    current_working_directory: AbsolutePath,
    #[cfg(test)]
    history: HashMap<&'static str, FromEditor>,
    #[cfg(test)]
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
    ) -> Self {
        Self {
            lsp_server_process_channels: HashMap::new(),
            sender,
            current_working_directory,
            #[cfg(test)]
            history: HashMap::new(),
            #[cfg(test)]
            lsp_server_initialized_args_history: Vec::new(),
        }
    }

    fn server_key(language: &Language, server_id: &str) -> Option<String> {
        Some(format!("{}:{server_id}", language.id()?))
    }

    fn is_lifecycle_message(message: &FromEditor) -> bool {
        matches!(
            message,
            FromEditor::TextDocumentDidOpen { .. }
                | FromEditor::TextDocumentDidChange { .. }
                | FromEditor::TextDocumentDidSave { .. }
                | FromEditor::WorkspaceDidRenameFiles { .. }
                | FromEditor::WorkspaceDidCreateFiles { .. }
        )
    }

    pub fn send_message(&mut self, path: AbsolutePath, message: FromEditor) -> anyhow::Result<()> {
        #[cfg(test)]
        self.history.insert(message.variant(), message.clone());

        let Some(language) = crate::config::from_path(&path) else {
            return Ok(());
        };
        crate::utils::consolidate_errors(
            "Failed to deliver LSP message",
            language
                .lsp_server_configs()
                .into_iter()
                .filter(|config| Self::is_lifecycle_message(&message) || config.primary())
                .filter_map(|config| {
                    let key = (
                        Self::server_key(&language, config.id())?,
                        root_for_path(&config, &path, &self.current_working_directory),
                    );
                    self.lsp_server_process_channels.get(&key)
                })
                .map(|channel| channel.send_from_editor(message.clone()))
                .collect(),
        )
    }

    /// Start configured servers or notify already initialized servers of the file.
    pub fn open_file(&mut self, path: AbsolutePath) -> anyhow::Result<()> {
        let Some(language) = crate::config::from_path(&path) else {
            return Ok(());
        };
        crate::utils::consolidate_errors(
            "Failed to open LSP document",
            language
                .lsp_server_configs()
                .into_iter()
                .map(|config| {
                    let Some(key) = Self::server_key(&language, config.id()) else {
                        return Ok(());
                    };
                    let key = (
                        key,
                        root_for_path(&config, &path, &self.current_working_directory),
                    );
                    if let Some(channel) = self.lsp_server_process_channels.get(&key) {
                        if channel.is_initialized() {
                            channel.document_did_open(path.clone())?;
                        }
                        return Ok(());
                    }
                    self.start_server(&language, config, key)
                })
                .collect(),
        )
    }

    fn start_server(
        &mut self,
        language: &Language,
        config: shared::language::LspServerConfig,
        key: (String, AbsolutePath),
    ) -> anyhow::Result<()> {
        let primary = config.primary();
        match LspServerProcessChannel::new(
            language.clone(),
            config,
            self.sender.clone(),
            key.1.clone(),
        ) {
            Ok(Some(channel)) => {
                self.lsp_server_process_channels.insert(key, channel);
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(error) if primary => Err(error),
            Err(error) => {
                log::warn!("Failed to start secondary LSP server {key:?}: {error:?}");
                Ok(())
            }
        }
    }

    pub fn initialized(
        &mut self,
        language: Language,
        server_id: String,
        root: AbsolutePath,
        documents: Vec<AbsolutePath>,
    ) {
        let Some(key) = Self::server_key(&language, &server_id) else {
            return;
        };
        let Some(config) = language
            .lsp_server_configs()
            .into_iter()
            .find(|config| config.id() == server_id)
        else {
            return;
        };
        let documents = documents
            .into_iter()
            .filter(|path| root_for_path(&config, path, &self.current_working_directory) == root)
            .collect::<Vec<_>>();
        #[cfg(test)]
        self.lsp_server_initialized_args_history
            .push((key.clone(), documents.clone()));
        let key = (key, root);
        if let Some(channel) = self.lsp_server_process_channels.get_mut(&key) {
            channel.initialized();
            if let Err(error) = channel.documents_did_open(documents) {
                log::error!("Failed to open documents on LSP server {key:?}: {error:?}");
            }
        }
    }

    pub fn shutdown(&mut self) {
        self.lsp_server_process_channels
            .drain()
            .for_each(|(_, channel)| {
                if let Err(error) = channel.shutdown() {
                    log::error!("{error:?}");
                }
            });
    }

    /// Restart each configured server and replay open documents on initialization.
    pub fn restart_language(&mut self, language: &Language) -> anyhow::Result<()> {
        let servers = language
            .lsp_server_configs()
            .into_iter()
            .flat_map(|config| {
                let keys = Self::server_key(language, config.id())
                    .map(|id| {
                        let keys = self
                            .lsp_server_process_channels
                            .keys()
                            .filter(|(key, _)| key == &id)
                            .cloned()
                            .collect::<Vec<_>>();
                        if keys.is_empty() {
                            vec![(id, self.current_working_directory.clone())]
                        } else {
                            keys
                        }
                    })
                    .unwrap_or_default();
                keys.into_iter().map(move |key| (config.clone(), key))
            })
            .collect::<Vec<_>>();
        crate::utils::consolidate_errors(
            "Failed to restart LSP servers",
            servers
                .into_iter()
                .map(|(config, key)| {
                    if let Some(channel) = self.lsp_server_process_channels.remove(&key) {
                        if let Err(error) = channel.shutdown() {
                            let _ = self.sender.send(AppMessage::LspNotification(Box::new(
                                LspNotification::Error(format!(
                                    "LSP server {key:?} failed to shut down: {error:?}"
                                )),
                            )));
                        }
                    }
                    self.start_server(language, config, key)
                })
                .collect(),
        )
    }

    #[cfg(test)]
    pub fn lsp_request_sent(&self, message: &FromEditor) -> bool {
        self.history.get(message.variant()) == Some(message)
    }

    #[cfg(test)]
    pub fn lsp_server_initialized_args(&self) -> Option<(String, Vec<AbsolutePath>)> {
        self.lsp_server_initialized_args_history.last().cloned()
    }
}
