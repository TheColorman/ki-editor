use crate::app::{RequestParams, Scope};
use crate::lsp::progress_notification_manager::ProgressNotificationManager;
use crate::thread::Callback;
use anyhow::Context;
use debounce::EventDebouncer;
use itertools::Itertools;
use lsp_types::notification::Notification;
use lsp_types::request::{
    GotoDeclarationParams, GotoImplementationParams, GotoTypeDefinitionParams, Request,
};
use lsp_types::*;
use my_proc_macros::NamedVariant;
use shared::absolute_path::AbsolutePath;
use shared::language::{Language, LspDiagnosticMode, LspServerConfig};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};

use std::process::{self};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::app::AppMessage;
use crate::lsp::server_config::resolve_initialization_options;
use crate::utils::consolidate_errors;

use super::code_action::CodeAction;
use super::completion::{Completion, CompletionItem};
use super::goto_definition_response::GotoDefinitionResponse;
use super::hover::Hover;
use super::prepare_rename_response::PrepareRenameResponse;
use super::signature_help::SignatureHelp;
use super::symbols::Symbols;
use super::workspace_edit::WorkspaceEdit;
use crate::quickfix_list::Location;

macro_rules! lsp_info {
    ($command:expr, $($arg:tt)*) => {
        log::info!("{{{}}} {}", $command, format_args!($($arg)*))
    };
}

macro_rules! lsp_error {
    ($command:expr, $($arg:tt)*) => {
        log::error!("{{{}}} {}", $command, format_args!($($arg)*))
    };
}

fn diagnostics_from_document_diagnostic(
    result: DocumentDiagnosticReportResult,
) -> Option<Vec<lsp_types::Diagnostic>> {
    match result {
        DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(report)) => {
            Some(report.full_document_diagnostic_report.items)
        }
        DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Unchanged(_)) => None,
        DocumentDiagnosticReportResult::Partial(_) => None,
    }
}

fn workspace_configuration_response(
    params: ConfigurationParams,
    root: &AbsolutePath,
) -> serde_json::Value {
    serde_json::Value::Array(
        params
            .items
            .into_iter()
            .map(|item| configuration_value(item.section.as_deref(), root))
            .collect(),
    )
}

fn configuration_value(section: Option<&str>, root: &AbsolutePath) -> serde_json::Value {
    let value = match section {
        Some("") => serde_json::json!({
            "typescript.tsdk": "${workspace}/node_modules/typescript/lib",
            "typescript.validate.enable": true,
            "javascript.validate.enable": true,
            "vtsls": {
                "tsserver": {
                    "globalPlugins": [{
                        "name": "@vue/typescript-plugin",
                        "location": "${vue_typescript_plugin}",
                        "languages": ["vue"],
                        "enableForWorkspaceTypeScriptVersions": true
                    }]
                }
            }
        }),
        Some("typescript") => serde_json::json!({
            "tsdk": "${workspace}/node_modules/typescript/lib",
            "validate": { "enable": true }
        }),
        Some("vtsls") => serde_json::json!({
            "tsserver": {
                "globalPlugins": [{
                    "name": "@vue/typescript-plugin",
                    "location": "${vue_typescript_plugin}",
                    "languages": ["vue"],
                    "enableForWorkspaceTypeScriptVersions": true
                }]
            }
        }),
        Some("eslint") => serde_json::json!({
            "enable": true,
            "run": "onType",
            "validate": ["javascript", "javascriptreact", "typescript", "typescriptreact", "vue"],
            "probe": ["javascript", "javascriptreact", "typescript", "typescriptreact", "vue"],
            "workingDirectories": [{ "mode": "auto" }]
        }),
        Some("eslint.enable") => serde_json::json!(true),
        Some("eslint.run") => serde_json::json!("onType"),
        Some("eslint.validate") => serde_json::json!([
            "javascript",
            "javascriptreact",
            "typescript",
            "typescriptreact",
            "vue"
        ]),
        Some("eslint.probe") => serde_json::json!([
            "javascript",
            "javascriptreact",
            "typescript",
            "typescriptreact",
            "vue"
        ]),
        Some("eslint.workingDirectories") => serde_json::json!([{ "mode": "auto" }]),
        _ => serde_json::Value::Null,
    };

    resolve_initialization_options(Some(value), root).unwrap_or(serde_json::Value::Null)
}

fn hint_for_lsp_error(message: &str) -> Option<&'static str> {
    if message.contains("Cannot find provider for definition") {
        Some(
            "For .vue files with vtsls, ensure the Vue TypeScript plugin is installed and configured for the server you are running. If using Nix-managed vtsls, make sure @vue/typescript-plugin is also available to that vtsls instance, or override the Vue lsp_servers initialization_options in Ki config to point to the plugin location.",
        )
    } else if message.contains("The \"path\" argument must be of type string") {
        Some(
            "The ESLint server failed while resolving a file path. Check that the ESLint server, eslint package, parser/plugins, and working-directory/config setup match your project. If using a non-node_modules install, ensure the server can still resolve the project eslint package and vue parser.",
        )
    } else if message.contains("Cannot find module") || message.contains("Failed to load plugin") {
        Some(
            "This usually means the language server could not resolve a required project package or plugin. Check the server command environment and package/plugin install location.",
        )
    } else {
        None
    }
}

struct LspServerProcess {
    language: Language,
    server_config: LspServerConfig,
    stdin: process::ChildStdin,

    /// This is hacky, but we need to keep the stdout around so that it doesn't get dropped
    stdout: Option<process::ChildStdout>,
    stderr: Option<process::ChildStderr>,

    server_capabilities: Option<ServerCapabilities>,
    current_working_directory: AbsolutePath,
    next_request_id: RequestId,
    pending_response_requests: HashMap<RequestId, PendingResponseRequest>,
    pending_call_hierarchy_directions: HashMap<RequestId, CallHierarchyDirection>,
    app_message_sender: crossbeam_channel::Sender<AppMessage>,

    sender: Sender<LspServerProcessMessage>,
    progress_notification_manager: ProgressNotificationManager,
    is_initialized: bool,
}

#[derive(Clone, Copy)]
struct ProcessGroup(u32);

impl ProcessGroup {
    fn kill(self) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            let pid = libc::pid_t::try_from(self.0).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "process ID is too large")
            })?;
            let result = unsafe { libc::kill(-pid, libc::SIGKILL) };
            if result == 0 {
                Ok(())
            } else {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
        #[cfg(not(unix))]
        {
            Ok(())
        }
    }
}

type RequestId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallHierarchyDirection {
    Incoming,
    Outgoing,
}

#[derive(Debug)]
struct PendingResponseRequest {
    method: String,
    context: ResponseContext,
    path: Option<AbsolutePath>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LspNotification {
    Initialized {
        language: Box<Language>,
        server_id: String,
        root: AbsolutePath,
    },
    PublishDiagnostics {
        server_id: String,
        params: PublishDiagnosticsParams,
    },
    Completion(ResponseContext, Completion),
    Hover(Hover),
    Definition(ResponseContext, GotoDefinitionResponse),
    References(ResponseContext, Vec<Location>),
    PrepareRenameResponse(PrepareRenameResponse),
    Error(String),
    WorkspaceEdit(WorkspaceEdit),
    CodeAction(Vec<CodeAction>),
    SignatureHelp(Option<SignatureHelp>),
    DocumentSymbols(Symbols),
    WorkspaceSymbols(Symbols),
    CompletionItemResolve(Box<lsp_types::CompletionItem>),
    Progress {
        message: String,
    },
    CallHierarchyIncomingCalls(ResponseContext, Vec<lsp_types::CallHierarchyIncomingCall>),
    CallHierarchyOutgoingCalls(ResponseContext, Vec<lsp_types::CallHierarchyOutgoingCall>),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResponseContext {
    pub scope: Option<Scope>,
    pub description: Option<String>,
}
impl ResponseContext {
    pub fn set_description(self, descrption: &str) -> Self {
        Self {
            description: Some(descrption.to_owned()),
            ..self
        }
    }
}

#[derive(Debug, Clone)]
enum LspServerProcessMessage {
    FromLspServer(serde_json::Value),
    /// This message might be throttled depending on its variant
    FromEditor(FromEditor),
    /// Throttled message should be executed immediately
    Throttled(FromEditor),
    Shutdown,
}

#[derive(Debug, NamedVariant, Clone, PartialEq)]
pub enum FromEditor {
    TextDocumentHover(RequestParams),
    TextDocumentCompletion(RequestParams),
    TextDocumentDefinition(RequestParams),
    TextDocumentReferences {
        params: RequestParams,
        include_declaration: bool,
    },
    TextDocumentDidOpen {
        file_path: AbsolutePath,
        language_id: String,
        version: usize,
        content: String,
    },
    TextDocumentDidChange {
        file_path: AbsolutePath,
        version: i32,
        content: String,
    },
    TextDocumentDidSave {
        file_path: AbsolutePath,
    },
    TextDocumentPrepareRename(RequestParams),
    TextDocumentRename {
        params: RequestParams,
        new_name: String,
    },
    TextDocumentCodeAction {
        params: RequestParams,
        diagnostics: Vec<lsp_types::Diagnostic>,
    },
    TextDocumentSignatureHelp(RequestParams),
    TextDocumentDeclaration(RequestParams),
    TextDocumentImplementation(RequestParams),
    TextDocumentTypeDefinition(RequestParams),
    TextDocumentDocumentSymbol(RequestParams),
    WorkspaceSymbol {
        query: String,
        context: ResponseContext,
    },
    WorkspaceDidRenameFiles {
        old: AbsolutePath,
        new: AbsolutePath,
    },
    WorkspaceDidCreateFiles {
        file_path: AbsolutePath,
    },
    WorkspaceExecuteCommand {
        params: RequestParams,
        command: super::code_action::Command,
    },
    CompletionItemResolve {
        completion_item: Box<lsp_types::CompletionItem>,
        params: RequestParams,
    },
    TextDocumentPrepareCallHierarchy {
        params: RequestParams,
        direction: CallHierarchyDirection,
    },
}

impl FromEditor {
    #[cfg(test)]
    pub fn variant(&self) -> &'static str {
        self.variant_name()
    }
}

pub struct LspServerProcessChannel {
    language: Language,
    server_config: LspServerConfig,
    sender: Sender<LspServerProcessMessage>,
    is_initialized: bool,
    child: Option<process::Child>,
    process_group: ProcessGroup,
}

impl Drop for LspServerProcessChannel {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = self.process_group.kill();
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl LspServerProcessChannel {
    pub fn new(
        language: Language,
        server_config: LspServerConfig,
        screen_message_sender: crossbeam_channel::Sender<AppMessage>,
        current_working_directory: AbsolutePath,
    ) -> Result<Option<LspServerProcessChannel>, anyhow::Error> {
        LspServerProcess::start(
            language,
            server_config,
            screen_message_sender,
            current_working_directory,
        )
    }

    pub fn request_shutdown(&self) -> anyhow::Result<()> {
        self.sender
            .send(LspServerProcessMessage::Shutdown)
            .map_err(|err| anyhow::anyhow!("Unable to send shutdown: {err}"))
    }

    pub fn wait_for_exit_until(mut self, deadline: Instant) {
        let Some(mut child) = self.child.take() else {
            return;
        };

        loop {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                Ok(None) => break,
                Err(error) => {
                    log::warn!("Failed to inspect LSP process: {error}");
                    break;
                }
            }
        }

        if let Err(error) = self.process_group.kill() {
            log::warn!("Failed to kill LSP process group: {error}");
        }
        let _ = child.kill();
        if let Err(error) = child.wait() {
            log::warn!("Failed to reap LSP process: {error}");
        }
    }

    fn send(&self, message: LspServerProcessMessage) -> anyhow::Result<()> {
        if !self.is_initialized {
            return Ok(());
        }
        self.sender
            .send(message)
            .map_err(|err| anyhow::anyhow!("Unable to send request: {}", err))
    }

    pub fn documents_did_open(&mut self, paths: Vec<AbsolutePath>) -> Result<(), anyhow::Error> {
        consolidate_errors(
            "[documents_did_open]",
            paths
                .into_iter()
                .map(|path| self.document_did_open(path))
                .collect(),
        )
    }

    pub fn document_did_open(&self, path: AbsolutePath) -> Result<(), anyhow::Error> {
        let content = path.read()?;
        let Some(language_id) = self
            .server_config
            .language_id()
            .or_else(|| self.language.id())
        else {
            return Ok(());
        };
        self.send(LspServerProcessMessage::FromEditor(
            FromEditor::TextDocumentDidOpen {
                file_path: path,
                language_id: language_id.to_string(),
                version: 1,
                content,
            },
        ))
    }

    pub fn is_initialized(&self) -> bool {
        self.is_initialized
    }

    pub fn initialized(&mut self) {
        self.is_initialized = true;
    }

    pub fn send_from_editor(&self, from_editor: FromEditor) -> Result<(), anyhow::Error> {
        self.send(LspServerProcessMessage::FromEditor(from_editor))
    }
}

impl LspServerProcess {
    fn start(
        language: Language,
        server_config: LspServerConfig,
        app_message_sender: crossbeam_channel::Sender<AppMessage>,
        current_working_directory: AbsolutePath,
    ) -> anyhow::Result<Option<LspServerProcessChannel>> {
        let process_command = server_config.process_command();

        let mut process = process_command
            .spawn_in_directory_in_new_process_group(current_working_directory.as_ref())?;
        let process_group = ProcessGroup(process.id());
        let stdin = process
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("Unable to obtain stdin"))?;

        let stderr = process
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("Unable to obtain stderr"))?;

        let stdout = process
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Unable to obtain stdout"))?;
        let (sender, receiver) = std::sync::mpsc::channel::<LspServerProcessMessage>();
        let mut lsp_server_process = LspServerProcess {
            language: language.clone(),
            server_config: server_config.clone(),
            stdin,
            stdout: Some(stdout),
            stderr: Some(stderr),
            current_working_directory,
            next_request_id: 0,
            pending_response_requests: HashMap::new(),
            pending_call_hierarchy_directions: HashMap::new(),
            server_capabilities: None,
            app_message_sender: app_message_sender.clone(),
            sender: sender.clone(),
            progress_notification_manager: ProgressNotificationManager::new(
                process_command.command().to_string(),
                Callback::new(Arc::new({
                    let app_message_sender = app_message_sender.clone();
                    move |message| {
                        let _ = app_message_sender.send(AppMessage::LspNotification(Box::new(
                            LspNotification::Progress { message },
                        )));
                    }
                })),
            ),
            is_initialized: false,
        };

        if let Err(error) = lsp_server_process.initialize() {
            let _ = process_group.kill();
            let _ = process.kill();
            let _ = process.wait();
            return Err(error);
        }

        std::thread::spawn(move || {
            if let Err(err) = lsp_server_process.listen(receiver, app_message_sender) {
                log::error!("Failed to start `lsp_server_process.listen` due to {err:?}");
            }
        });

        Ok(Some(LspServerProcessChannel {
            language,
            server_config,
            sender,
            is_initialized: false,
            child: Some(process),
            process_group,
        }))
    }

    #[allow(deprecated)]
    fn initialize(&mut self) -> anyhow::Result<()> {
        self.send_request::<lsp_request!("initialize")>(
            ResponseContext::default(),
            None,
            InitializeParams {
                process_id: None,
                initialization_options: resolve_initialization_options(
                    self.server_config.initialization_options(),
                    &self.current_working_directory,
                ),
                capabilities: ClientCapabilities {
                    workspace: Some(WorkspaceClientCapabilities {
                        apply_edit: Some(true),
                        workspace_edit: Some(WorkspaceEditClientCapabilities {
                            document_changes: Some(true),
                            resource_operations: Some(
                                [
                                    ResourceOperationKind::Rename,
                                    ResourceOperationKind::Create,
                                    ResourceOperationKind::Delete,
                                ]
                                .into_iter()
                                .collect(),
                            ),
                            ..WorkspaceEditClientCapabilities::default()
                        }),
                        file_operations: Some(WorkspaceFileOperationsClientCapabilities {
                            did_rename: Some(true),
                            did_create: Some(true),
                            ..Default::default()
                        }),
                        execute_command: Some(DynamicRegistrationClientCapabilities {
                            dynamic_registration: None,
                        }),
                        configuration: Some(true),
                        symbol: Some(WorkspaceSymbolClientCapabilities {
                            ..Default::default()
                        }),
                        ..WorkspaceClientCapabilities::default()
                    }),
                    window: Some(WindowClientCapabilities {
                        work_done_progress: Some(true),
                        ..Default::default()
                    }),
                    text_document: Some(TextDocumentClientCapabilities {
                        publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                            related_information: Some(true),
                            tag_support: Some(TagSupport {
                                value_set: vec![
                                    DiagnosticTag::DEPRECATED,
                                    DiagnosticTag::UNNECESSARY,
                                ],
                            }),
                            code_description_support: Some(true),
                            ..PublishDiagnosticsClientCapabilities::default()
                        }),
                        diagnostic: Some(DiagnosticClientCapabilities {
                            dynamic_registration: Some(false),
                            related_document_support: Some(false),
                        }),
                        completion: Some(CompletionClientCapabilities {
                            completion_item: Some(CompletionItemCapability {
                                resolve_support: Some(CompletionItemCapabilityResolveSupport {
                                    properties: vec![
                                        "textEdit".to_string(),
                                        "additionalTextEdits".to_string(),
                                    ],
                                }),

                                ..CompletionItemCapability::default()
                            }),
                            completion_item_kind: Some(CompletionItemKindCapability {
                                ..Default::default()
                            }),
                            ..CompletionClientCapabilities::default()
                        }),
                        hover: Some(HoverClientCapabilities {
                            content_format: Some(vec![MarkupKind::PlainText]),
                            ..HoverClientCapabilities::default()
                        }),
                        code_action: Some(CodeActionClientCapabilities {
                            code_action_literal_support: Some(CodeActionLiteralSupport {
                                code_action_kind: CodeActionKindLiteralSupport {
                                    value_set: vec![
                                        CodeActionKind::EMPTY,
                                        CodeActionKind::QUICKFIX,
                                        CodeActionKind::REFACTOR,
                                        CodeActionKind::REFACTOR_EXTRACT,
                                        CodeActionKind::REFACTOR_INLINE,
                                        CodeActionKind::REFACTOR_REWRITE,
                                        CodeActionKind::SOURCE,
                                        CodeActionKind::SOURCE_ORGANIZE_IMPORTS,
                                        CodeActionKind::SOURCE_FIX_ALL,
                                    ]
                                    .into_iter()
                                    .map(|kind| kind.as_str().to_string())
                                    .collect(),
                                },
                            }),
                            ..Default::default()
                        }),
                        rename: Some(RenameClientCapabilities {
                            prepare_support: Some(true),
                            ..Default::default()
                        }),
                        signature_help: Some(SignatureHelpClientCapabilities {
                            signature_information: Some(SignatureInformationSettings {
                                documentation_format: Some(vec![MarkupKind::PlainText]),
                                parameter_information: Some(ParameterInformationSettings {
                                    label_offset_support: Some(true),
                                }),
                                active_parameter_support: Some(true),
                            }),
                            ..Default::default()
                        }),
                        declaration: Some(GotoCapability {
                            dynamic_registration: Some(true),
                            link_support: None,
                        }),
                        call_hierarchy: Some(CallHierarchyClientCapabilities {
                            dynamic_registration: Some(true),
                        }),
                        ..TextDocumentClientCapabilities::default()
                    }),
                    ..ClientCapabilities::default()
                },
                root_uri: Some(
                    Url::from_file_path(self.current_working_directory.as_ref())
                        .map_err(|_| anyhow::anyhow!("Unable to create LSP root URI"))?,
                ),
                workspace_folders: Some(vec![WorkspaceFolder {
                    uri: Url::from_file_path(self.current_working_directory.as_ref())
                        .map_err(|_| anyhow::anyhow!("Unable to create LSP workspace URI"))?,
                    name: "root".to_string(),
                }]),
                ..InitializeParams::default()
            },
        )?;
        Ok(())
    }

    /// Main orchestrator that starts two concurrent loops:
    /// 1. Spawns a thread to continuously read from LSP server's stdout
    /// 2. Processes incoming messages from both the stdout reader and editor
    ///
    /// Returns the stdout reader thread handle for cleanup
    pub fn listen(
        mut self,
        receiver: Receiver<LspServerProcessMessage>,
        app_message_sender: crossbeam_channel::Sender<AppMessage>,
    ) -> anyhow::Result<JoinHandle<()>> {
        let lsp_command = self.lsp_command();
        let stdout_reader = BufReader::new(
            self.stdout
                .take()
                .ok_or(anyhow::anyhow!("Failed to obtain stdout"))?,
        );
        let stderr_reader = BufReader::new(
            self.stderr
                .take()
                .ok_or(anyhow::anyhow!("Failed to obtain stderr"))?,
        );
        let sender = self.sender.clone();

        // Start the stdout reader loop in its own thread
        let _stderr_handle = Self::spawn_stderr_reader(stderr_reader, lsp_command.clone());
        let stdout_handle = self.spawn_stdout_reader(
            stdout_reader,
            sender.clone(),
            app_message_sender.clone(),
            lsp_command,
        );

        // Start the message processor loop in the main thread
        lsp_info!(
            self.lsp_command(),
            "[LspServerProcess] Listening for messages from LSP server"
        );
        self.process_messages(receiver);
        lsp_info!(
            self.lsp_command(),
            "LspServerProcess::listen | Stopped listening for messages from LSP server"
        );

        Ok(stdout_handle)
    }

    /// Runs a loop that reads raw LSP protocol messages from stdout
    /// Handles error tracking/recovery and sends parsed messages to the message processor
    /// Sends shutdown signal if too many errors occur
    fn spawn_stdout_reader(
        &self,
        mut stdout_reader: BufReader<process::ChildStdout>,
        sender: Sender<LspServerProcessMessage>,
        app_message_sender: crossbeam_channel::Sender<AppMessage>,
        lsp_command: String,
    ) -> JoinHandle<()> {
        thread::spawn(move || {
            let mut error_tracker = ErrorTracker::new();

            // The stdout reader loop
            loop {
                match Self::read_response(&mut stdout_reader, &sender) {
                    Ok(()) => error_tracker.handle_success(),
                    Err(error) => {
                        lsp_error!(
                            lsp_command,
                            "[LspServerProcess] read_response error = {error:?}"
                        );
                        if !error_tracker.handle_error(error, &sender) {
                            let formatted_errors = error_tracker
                                .consecutive_errors
                                .iter()
                                .enumerate()
                                .map(|(index, error)| format!("Error #{}: {}", index + 1, error))
                                .collect_vec()
                                .join("\n");
                            let error = if error_tracker.consecutive_errors.len()
                                >= ErrorTracker::MAX_CONSECUTIVE_ERRORS
                            {
                                format!(
                                    "LspServerProcess::listen: Stopping LSP command:\n\n`{}`\n\nToo many consecutive errors ({}):\n{}",
                                    lsp_command,
                                    ErrorTracker::MAX_CONSECUTIVE_ERRORS,
                                    formatted_errors
                                )
                            } else {
                                format!(
                                    "LspServerProcess::listen: Stopping LSP command:\n\n`{}`\n\nLSP server stopped after error:\n{}",
                                    lsp_command, formatted_errors
                                )
                            };
                            app_message_sender
                                .send(AppMessage::LspNotification(Box::new(
                                    LspNotification::Error(error),
                                )))
                                .unwrap_or_else(|error| {
                                    lsp_error!(
                                        lsp_command,
                                        "[LspServerProcess] Error sending error to app: {error:?}"
                                    );
                                });
                            sender
                                .send(LspServerProcessMessage::Shutdown)
                                .unwrap_or_else(|error| {
                                    lsp_error!(
                                        lsp_command,
                                        "[LspServerProcess] Error sending Shutdown to the loop outside: {error:?}"
                                    );
                                });
                            break;
                        }
                    }
                }
            }
        })
    }

    fn spawn_stderr_reader(
        mut stderr_reader: BufReader<process::ChildStderr>,
        lsp_command: String,
    ) -> JoinHandle<()> {
        thread::spawn(move || {
            let mut buffer = [0_u8; 8192];
            loop {
                match stderr_reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(length) => lsp_error!(
                        lsp_command,
                        "stderr: {}",
                        String::from_utf8_lossy(&buffer[..length])
                    ),
                    Err(error) => {
                        lsp_error!(lsp_command, "Failed to read stderr: {error}");
                        break;
                    }
                }
            }
        })
    }

    /// Processes all incoming messages:
    /// - LSP server responses (from stdout reader)
    /// - Editor requests (e.g. completions, hover)
    /// - Throttled requests (debounced editor actions)
    ///
    /// Breaks loop when shutdown message received
    fn process_messages(&mut self, receiver: Receiver<LspServerProcessMessage>) {
        // Set up event debouncing
        struct Event(FromEditor);
        impl PartialEq for Event {
            fn eq(&self, other: &Self) -> bool {
                self.0.variant_name() == other.0.variant_name()
            }
        }

        let debounce = {
            let sender = self.sender.clone();

            EventDebouncer::new(Duration::from_millis(150), move |Event(from_editor)| {
                sender
                .send(LspServerProcessMessage::Throttled(from_editor.clone()))
                .unwrap_or_else(|error| {
                    log::info!("LspServerProcess::listen::debounce | Error sending throttled message from_editor={from_editor:?}, error={error:?}");
                });
            })
        };

        // The message processor loop
        while let Ok(message) = receiver.recv() {
            match &message {
                LspServerProcessMessage::FromLspServer(json_value) => {
                    self.handle_reply(json_value.clone())
                    .unwrap_or_else(|error| {
                        lsp_info!(
                            self.lsp_command(),"LspServerProcess::listen | Error handling reply from LSP server, json={json_value:?}, error={error:?}"
                        );
                    });
                }
                LspServerProcessMessage::FromEditor(from_editor) => match from_editor.clone() {
                    FromEditor::CompletionItemResolve {
                        completion_item,
                        params,
                    } => debounce.put(Event(FromEditor::CompletionItemResolve {
                        completion_item,
                        params,
                    })),
                    _ => self.handle_from_editor(from_editor),
                },
                LspServerProcessMessage::Throttled(from_editor) => {
                    self.handle_from_editor(from_editor);
                }
                LspServerProcessMessage::Shutdown => {
                    if let Err(err) = self.shutdown() {
                        lsp_error!(
                            self.lsp_command(),
                            "LspServerProcess::process_messages: failed to shutdown due to {err:?}"
                        );
                    }
                    break;
                }
            }
        }
    }

    /// Handles low-level LSP protocol message parsing:
    /// 1. Reads Content-Length header
    /// 2. Reads message content
    /// 3. Parses JSON
    /// 4. Sends parsed message back via channel
    fn read_response(
        reader: &mut BufReader<process::ChildStdout>,
        sender: &Sender<LspServerProcessMessage>,
    ) -> anyhow::Result<()> {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .with_context(|| "Failed to read Content-Length")?;
        if line.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "LSP server stdout closed",
            )
            .into());
        }

        let content_length = line
            .split(':')
            .nth(1)
            .ok_or_else(|| {
                anyhow::anyhow!("Parsing Content-Length: Unable to split line: {line:?}")
            })?
            .trim()
            .parse::<usize>()
            .with_context(|| "Parsing Content-Length: Failed to parse number.")?;

        // According to https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#headerPart,
        // we need to loop until we encounter an empty line, because the JSON comes after the empty line.
        loop {
            line.clear();
            reader
                .read_line(&mut line)
                .with_context(|| "Failed to read content.")?;
            if line == "\r\n" {
                break;
            }
        }

        let mut buffer = vec![0; content_length];
        reader
            .read_exact(&mut buffer)
            .with_context(|| "Failed to read buffer into vector.")?;

        let reply = String::from_utf8(buffer)
            .with_context(|| "Failed to convert content buffer into String.")?;

        let reply: serde_json::Value = serde_json::from_str(&reply).map_err(|err| {
            anyhow::anyhow!(
                "Failed to convert content string into JSON value due to error: {err:?}. Content is {reply:?}"
            )
        })?;

        sender
            .send(LspServerProcessMessage::FromLspServer(reply))
            .unwrap_or_else(|error| {
                lsp_error!(
                    "{unknown LSP command}",
                    "[LspServerProcess] Error sending reply: {error:?}"
                );
            });

        Ok(())
    }

    fn handle_reply(&mut self, reply: serde_json::Value) -> anyhow::Result<()> {
        // Check if reply is Response or Notification
        // Only Notification contains the `method` field
        if let Some(error) = reply.get("error") {
            let message = error
                .get("message")
                .and_then(|message| message.as_str())
                .unwrap_or("<missing error message>");
            let method = reply
                .get("id")
                .and_then(|id| id.as_u64())
                .and_then(|id| self.pending_response_requests.get(&id))
                .map(|request| request.method.as_str())
                .unwrap_or("<unknown request>");
            if let Some(hint) = hint_for_lsp_error(message) {
                lsp_error!(
                    self.lsp_command(),
                    "LSP request {method} failed: {message}\nHint: {hint}"
                );
            } else {
                lsp_error!(self.lsp_command(), "LSP request {method} failed: {message}");
            }
            return Err(anyhow::anyhow!("Reply contains field `error`."));
        }
        match reply.get("method") {
            // reply is Response
            None => {
                // Get the request ID
                let request_id = reply
                    .get("id")
                    .ok_or_else(|| anyhow::anyhow!("Unable to obtain ID from reply: {reply:#?}"))?;

                let request_id = request_id
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("Unable to convert {request_id:#?} to u64"))?;

                // Get the method of the request
                let pending_response_request = self
                    .pending_response_requests
                    .remove(&request_id)
                    .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Unable to get pending response requests for request ID {request_id:#?}"
                    )
                })?;

                // Parse the reply as a Response
                let response = serde_json::from_value::<
                    json_rpc_types::Response<
                        serde_json::Value,
                        (),
                        // Need to specify String here
                        // Otherwise the default will be `str_buf::StrBuf<31>`,
                        // which says the error message can only be 31 bytes long.
                        String,
                    >,
                >(reply)
                .map_err(|e| anyhow::anyhow!("Serde error = {:?}", e))?
                .payload
                .map_err(|e| {
                    self.send_to_app(AppMessage::LspNotification(Box::new(
                        LspNotification::Error(format!(
                            "LSP JSON-RPC Error: {:?}: {}",
                            e.code, e.message
                        )),
                    )));
                    anyhow::anyhow!(
                        "LSP JSON-RPC Error: Code={:?} Message={}",
                        e.code,
                        e.message
                    )
                })?;

                let PendingResponseRequest {
                    method,
                    context: response_context,
                    path,
                } = pending_response_request;

                lsp_info!(
                    self.lsp_command(),
                    "LspServerProcess::handle_reply: {}",
                    method.as_str()
                );

                match method.as_str() {
                    "initialize" => {
                        lsp_info!(self.lsp_command(), "Initialize response: {response:?}");
                        let payload: <lsp_request!("initialize") as Request>::Result =
                            serde_json::from_value(response)?;

                        // Get the capabilities
                        self.server_capabilities = Some(payload.capabilities);

                        // Send the initialized notification
                        self.send_notification::<lsp_notification!("initialized")>(
                            InitializedParams {},
                        )?;
                        self.is_initialized = true;

                        self.app_message_sender
                            .send(AppMessage::LspNotification(Box::new(
                                LspNotification::Initialized {
                                    language: Box::new(self.language.clone()),
                                    server_id: self.server_config.id().to_string(),
                                    root: self.current_working_directory.clone(),
                                },
                            )))?;
                    }
                    "textDocument/completion" => {
                        let payload: <lsp_request!("textDocument/completion") as Request>::Result =
                            serde_json::from_value(response)?;

                        if let Some(payload) = payload {
                            self.send_to_app(AppMessage::LspNotification(Box::new(
                                LspNotification::Completion(
                                    response_context,
                                    Completion {
                                        trigger_characters: self.trigger_characters(),
                                        items: match payload {
                                            CompletionResponse::Array(items) => items,
                                            CompletionResponse::List(list) => list.items,
                                        }
                                        .into_iter()
                                        .map(CompletionItem::from)
                                        .map(|item| item.into())
                                        .collect(),
                                    },
                                ),
                            )));
                        }
                    }
                    "textDocument/hover" => {
                        let payload: <lsp_request!("textDocument/hover") as Request>::Result =
                            serde_json::from_value(response)?;

                        if let Some(payload) = payload {
                            self.send_to_app(AppMessage::LspNotification(Box::new(
                                LspNotification::Hover(payload.into()),
                            )));
                        }
                    }
                    "textDocument/diagnostic" => {
                        let payload: <lsp_request!("textDocument/diagnostic") as Request>::Result =
                            serde_json::from_value(response)?;
                        let Some(path) = path else {
                            return Ok(());
                        };
                        if let Some(diagnostics) = diagnostics_from_document_diagnostic(payload) {
                            self.send_to_app(AppMessage::LspNotification(Box::new(
                                LspNotification::PublishDiagnostics {
                                    server_id: self.server_config.id().to_string(),
                                    params: PublishDiagnosticsParams {
                                        uri: path_buf_to_url(path)?,
                                        diagnostics,
                                        version: None,
                                    },
                                },
                            )));
                        }
                    }
                    "textDocument/definition" => {
                        let payload: <lsp_request!("textDocument/definition") as Request>::Result =
                            serde_json::from_value(response)?;

                        if let Some(payload) = payload {
                            self.send_to_app(AppMessage::LspNotification(Box::new(
                                LspNotification::Definition(response_context, payload.try_into()?),
                            )));
                        }
                    }
                    "textDocument/references" => {
                        let payload: <lsp_request!("textDocument/references") as Request>::Result =
                            serde_json::from_value(response)?;

                        if let Some(payload) = payload {
                            self.send_to_app(AppMessage::LspNotification(Box::new(
                                LspNotification::References(
                                    response_context,
                                    payload
                                        .into_iter()
                                        .map(|r| r.try_into())
                                        .collect::<Result<Vec<_>, _>>()?,
                                ),
                            )));
                        }
                    }
                    "textDocument/declaration" => {
                        let payload: <lsp_request!("textDocument/declaration") as Request>::Result =
                            serde_json::from_value(response)?;

                        if let Some(payload) = payload {
                            self.send_to_app(AppMessage::LspNotification(Box::new(
                                LspNotification::Definition(response_context, payload.try_into()?),
                            )));
                        }
                    }
                    "textDocument/typeDefinition" => {
                        let payload: <lsp_request!("textDocument/typeDefinition") as Request>::Result =
                            serde_json::from_value(response)?;

                        if let Some(payload) = payload {
                            self.send_to_app(AppMessage::LspNotification(Box::new(
                                LspNotification::Definition(response_context, payload.try_into()?),
                            )));
                        }
                    }
                    "textDocument/implementation" => {
                        let payload: <lsp_request!("textDocument/implementation") as Request>::Result =
                            serde_json::from_value(response)?;

                        if let Some(payload) = payload {
                            self.send_to_app(AppMessage::LspNotification(Box::new(
                                LspNotification::Definition(response_context, payload.try_into()?),
                            )));
                        }
                    }
                    "textDocument/prepareRename" => {
                        let payload: <lsp_request!("textDocument/prepareRename") as Request>::Result =
                            serde_json::from_value(response)?;

                        if let Some(payload) = payload {
                            self.send_to_app(AppMessage::LspNotification(Box::new(
                                LspNotification::PrepareRenameResponse(payload.into()),
                            )));
                        }
                    }
                    "textDocument/rename" => {
                        let payload: <lsp_request!("textDocument/rename") as Request>::Result =
                            serde_json::from_value(response)?;

                        if let Some(payload) = payload {
                            self.send_to_app(AppMessage::LspNotification(Box::new(
                                LspNotification::WorkspaceEdit(payload.try_into()?),
                            )));
                        }
                    }
                    "textDocument/codeAction" => {
                        let payload: <lsp_request!("textDocument/codeAction") as Request>::Result =
                            serde_json::from_value(response)?;

                        if let Some(payload) = payload {
                            self.send_to_app(AppMessage::LspNotification(Box::new(
                                LspNotification::CodeAction(
                                    payload
                                        .into_iter()
                                        .map(|r| match r {
                                            CodeActionOrCommand::Command(_) => todo!(),
                                            CodeActionOrCommand::CodeAction(code_action) => {
                                                code_action.try_into()
                                            }
                                        })
                                        .collect::<Result<Vec<_>, _>>()?,
                                ),
                            )));
                        }
                    }
                    "textDocument/signatureHelp" => {
                        let payload: <lsp_request!("textDocument/signatureHelp") as Request>::Result =
                            serde_json::from_value(response)?;

                        self.send_to_app(AppMessage::LspNotification(Box::new(
                            LspNotification::SignatureHelp(payload.map(|payload| payload.into())),
                        )));
                    }
                    "textDocument/documentSymbol" => {
                        let payload: <lsp_request!("textDocument/documentSymbol") as Request>::Result =
                            serde_json::from_value(response)?;

                        if let Some(payload) = payload {
                            if let Some(path) = path {
                                self.send_to_app(AppMessage::LspNotification(Box::new(
                                    LspNotification::DocumentSymbols(
                                        Symbols::try_from_document_symbol_response(payload, path)?,
                                    ),
                                )));
                            }
                        }
                    }
                    "completionItem/resolve" => {
                        let payload: <lsp_request!("completionItem/resolve") as Request>::Result =
                            serde_json::from_value(response)?;

                        self.send_to_app(AppMessage::LspNotification(Box::new(
                            LspNotification::CompletionItemResolve(Box::new(payload)),
                        )));
                    }
                    "workspace/symbol" => {
                        let payload: <lsp_request!("workspace/symbol") as Request>::Result =
                            serde_json::from_value(response)?;

                        if let Some(workspace_symbol_response) = payload {
                            let symbols = Symbols::try_from_workspace_symbol_response(
                                workspace_symbol_response,
                                &self.current_working_directory,
                            )?;

                            self.send_to_app(AppMessage::LspNotification(Box::new(
                                LspNotification::WorkspaceSymbols(symbols),
                            )));
                        }
                    }
                    "textDocument/prepareCallHierarchy" => {
                        let payload: <lsp_request!("textDocument/prepareCallHierarchy") as Request>::Result =
                            serde_json::from_value(response)?;

                        if let Some(item) = payload.and_then(|items| items.into_iter().next()) {
                            let direction = self
                                .pending_call_hierarchy_directions
                                .remove(&request_id)
                                .unwrap_or(CallHierarchyDirection::Incoming);
                            match direction {
                                CallHierarchyDirection::Incoming => {
                                    self.call_hierarchy_incoming_calls(response_context, item)?;
                                }
                                CallHierarchyDirection::Outgoing => {
                                    self.call_hierarchy_outgoing_calls(response_context, item)?;
                                }
                            }
                        }
                    }
                    "callHierarchy/incomingCalls" => {
                        let payload: <lsp_request!("callHierarchy/incomingCalls") as Request>::Result =
                            serde_json::from_value(response)?;

                        if let Some(calls) = payload {
                            self.send_to_app(AppMessage::LspNotification(Box::new(
                                LspNotification::CallHierarchyIncomingCalls(
                                    response_context,
                                    calls,
                                ),
                            )));
                        }
                    }
                    "callHierarchy/outgoingCalls" => {
                        let payload: <lsp_request!("callHierarchy/outgoingCalls") as Request>::Result =
                            serde_json::from_value(response)?;

                        if let Some(calls) = payload {
                            self.send_to_app(AppMessage::LspNotification(Box::new(
                                LspNotification::CallHierarchyOutgoingCalls(
                                    response_context,
                                    calls,
                                ),
                            )));
                        }
                    }
                    _ => {
                        lsp_info!(self.lsp_command(), "Unknown method: {method:#?}");
                    }
                }
            }

            // reply is Notification
            Some(_) => {
                let request = serde_json::from_value::<
                    json_rpc_types::Request<
                        serde_json::Value,
                        // Need to specify String here
                        // Otherwise the default will be `str_buf::StrBuf<31>`,
                        // which says the error message can only be 31 bytes long.
                        String,
                    >,
                >(reply)
                .map_err(|e| anyhow::anyhow!("Serde error = {:?}", e))?;

                let method = request.method.clone();
                // Parse the reply as Notification
                if method.as_str() != "$/progress" {
                    lsp_info!(
                        self.lsp_command(),
                        "LspServerProcess::handle_notification: {}",
                        method.as_str()
                    );
                }
                match method.as_str() {
                    "textDocument/publishDiagnostics" => {
                        let params: <lsp_notification!("textDocument/publishDiagnostics") as Notification>::Params =
                            serde_json::from_value(request.params.ok_or_else(|| anyhow::anyhow!("Missing params"))?)?;

                        self.send_to_app(AppMessage::LspNotification(Box::new(
                            LspNotification::PublishDiagnostics {
                                server_id: self.server_config.id().to_string(),
                                params,
                            },
                        )));
                    }
                    "workspace/applyEdit" => {
                        let params: <lsp_request!("workspace/applyEdit") as Request>::Params =
                            serde_json::from_value(request.clone().params.ok_or_else(|| {
                                anyhow::anyhow!(
                                    "Unable to obtain request.params from request {request:#?}"
                                )
                            })?)?;

                        self.send_to_app(AppMessage::LspNotification(Box::new(
                            LspNotification::WorkspaceEdit(params.edit.try_into()?),
                        )));
                    }
                    "workspace/configuration" => {
                        let params: ConfigurationParams = serde_json::from_value(
                            request
                                .params
                                .ok_or_else(|| anyhow::anyhow!("Missing params"))?,
                        )?;
                        self.send_reply(
                            request.id,
                            workspace_configuration_response(
                                params,
                                &self.current_working_directory,
                            ),
                        )?;
                    }
                    "window/workDoneProgress/create" => {
                        // This reply is necessary for the Go LSP (gopls) to work
                        // Null as the response is fine but maybe this should be handled properly
                        self.send_reply(request.id, serde_json::Value::Null)?;
                    }
                    "window/logMessage" => {
                        let params: <lsp_notification!("window/logMessage") as Notification>::Params =
                            serde_json::from_value(request.params.ok_or_else(|| anyhow::anyhow!("Missing params"))?)?;
                        let typ = match params.typ {
                            MessageType::LOG => "LOG".to_string(),
                            MessageType::ERROR => "ERROR".to_string(),
                            MessageType::WARNING => "WARNING".to_string(),
                            MessageType::INFO => "INFO".to_string(),
                            _ => format!("[Unknown message type {:?}]", params.typ),
                        };
                        lsp_info!(
                            self.lsp_command(),
                            "LSP(window/logMessage)[{typ}]: '{}'",
                            params.message
                        );
                    }
                    "$/progress" => {
                        let params: <lsp_notification!("$/progress") as Notification>::Params =
                            serde_json::from_value(
                                request
                                    .params
                                    .ok_or_else(|| anyhow::anyhow!("Missing params"))?,
                            )?;
                        self.handle_progress_notification(params);
                    }

                    _ => lsp_info!(
                        self.lsp_command(),
                        "unhandled Incoming Notification: {method}"
                    ),
                }
            }
        }

        Ok(())
    }

    fn trigger_characters(&self) -> Vec<String> {
        self.server_capabilities
            .as_ref()
            .and_then(|capabilities| {
                capabilities
                    .completion_provider
                    .as_ref()
                    .and_then(|provider| provider.trigger_characters.clone())
            })
            .unwrap_or_default()
    }

    pub fn shutdown(&mut self) -> anyhow::Result<()> {
        if self.is_initialized {
            let shutdown =
                self.send_request::<lsp_request!("shutdown")>(ResponseContext::default(), None, ());
            let exit = self.send_notification::<lsp_notification!("exit")>(());
            shutdown.and(exit)?;
        }
        Ok(())
    }

    fn send_notification<N: Notification>(&mut self, params: N::Params) -> anyhow::Result<()> {
        let notification = json_rpc_types::Request {
            id: None,
            jsonrpc: json_rpc_types::Version::V2,
            method: N::METHOD,
            params: Some(params),
        };

        lsp_info!(
            self.lsp_command(),
            "Sending notification: {:?} {:?}",
            self.language.id(),
            N::METHOD
        );

        self.send_json(&notification)?;

        Ok(())
    }

    /// Used for sending response to reponses of the LSP server
    fn send_reply(
        &mut self,
        id: Option<json_rpc_types::Id>,
        result: serde_json::Value,
    ) -> anyhow::Result<()> {
        /// Refer https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#responseMessage
        #[derive(serde::Serialize)]
        struct ResponseMessage {
            jsonrpc: &'static str,
            id: Option<json_rpc_types::Id>,
            result: serde_json::Value,
        }
        let request = ResponseMessage {
            jsonrpc: "2.0",
            id,
            result,
        };
        self.send_json(&request)?;

        Ok(())
    }

    /// Send JSON to the LSP server by writing to the server's stdin
    fn send_json<T: serde::Serialize>(&mut self, value: T) -> anyhow::Result<()> {
        let json = serde_json::to_string(&value)?;

        // The message format is according to https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#contentPart
        write!(
            &mut self.stdin,
            "Content-Length: {}\r\n\r\n{}",
            json.len(),
            json
        )?;
        Ok(())
    }

    /// Returns the request ID
    fn send_request<R: Request>(
        &mut self,
        context: ResponseContext,
        path: Option<AbsolutePath>,
        params: R::Params,
    ) -> anyhow::Result<()>
    where
        R::Params: serde::Serialize,
    {
        let id = {
            let result = self.next_request_id;
            self.next_request_id += 1;
            result
        };
        // Convert the request to a JSON-RPC message
        let request = json_rpc_types::Request {
            jsonrpc: json_rpc_types::Version::V2,
            method: R::METHOD,
            params: Some(params),
            id: Some(json_rpc_types::Id::Num(id)),
        };

        self.send_json(&request)?;

        self.pending_response_requests.insert(
            id,
            PendingResponseRequest {
                context,
                method: R::METHOD.to_string(),
                path,
            },
        );

        Ok(())
    }

    fn text_document_did_open(
        &mut self,
        file_path: AbsolutePath,
        language_id: String,
        version: usize,
        content: String,
    ) -> Result<(), anyhow::Error> {
        if !self.has_capability(Self::server_supports_text_document_open_close) {
            return Ok(());
        }

        self.send_notification::<lsp_notification!("textDocument/didOpen")>(
            DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: path_buf_to_url(file_path.clone())?,
                    language_id,
                    version: version as i32,
                    text: content,
                },
            },
        )?;
        self.request_document_diagnostics(file_path)
    }

    fn text_document_did_change(
        &mut self,
        file_path: AbsolutePath,
        version: i32,
        content: String,
    ) -> Result<(), anyhow::Error> {
        if !self.has_capability(Self::server_supports_text_document_change) {
            return Ok(());
        }

        self.send_notification::<lsp_notification!("textDocument/didChange")>(
            DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: path_buf_to_url(file_path.clone())?,
                    version,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: content,
                }],
            },
        )?;
        self.request_document_diagnostics(file_path)
    }

    fn text_document_did_save(&mut self, file_path: AbsolutePath) -> Result<(), anyhow::Error> {
        if !self.has_capability(Self::server_supports_text_document_save) {
            return Ok(());
        }

        self.send_notification::<lsp_notification!("textDocument/didSave")>(
            DidSaveTextDocumentParams {
                text_document: path_buf_to_text_document_identifier(file_path.clone())?,
                text: None,
            },
        )?;
        self.request_document_diagnostics(file_path)
    }

    fn request_document_diagnostics(
        &mut self,
        file_path: AbsolutePath,
    ) -> Result<(), anyhow::Error> {
        if !self.server_supports_pull_diagnostics() {
            return Ok(());
        }
        self.send_request::<lsp_request!("textDocument/diagnostic")>(
            ResponseContext::default(),
            Some(file_path.clone()),
            DocumentDiagnosticParams {
                text_document: path_buf_to_text_document_identifier(file_path)?,
                identifier: None,
                previous_result_id: None,
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            },
        )
    }

    fn server_supports_pull_diagnostics(&self) -> bool {
        matches!(
            self.server_config.diagnostic_mode(),
            LspDiagnosticMode::Pull | LspDiagnosticMode::Both
        ) && self
            .server_capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.diagnostic_provider.is_some())
    }

    fn workspace_did_rename_files(
        &mut self,
        old: AbsolutePath,
        new: AbsolutePath,
    ) -> Result<(), anyhow::Error> {
        if !self.has_capability(Self::server_supports_workspace_did_rename_files) {
            return Ok(());
        }

        self.send_notification::<lsp_notification!("workspace/didRenameFiles")>(RenameFilesParams {
            files: [FileRename {
                old_uri: old.display_absolute(),
                new_uri: new.display_absolute(),
            }]
            .to_vec(),
        })
    }

    fn workspace_did_create_files(&mut self, file_path: AbsolutePath) -> Result<(), anyhow::Error> {
        if !self.has_capability(Self::server_supports_workspace_did_create_files) {
            return Ok(());
        }

        self.send_notification::<lsp_notification!("workspace/didCreateFiles")>(CreateFilesParams {
            files: [FileCreate {
                uri: file_path.display_absolute(),
            }]
            .to_vec(),
        })
    }

    fn has_capability(&self, f: impl Fn(&ServerCapabilities) -> bool) -> bool {
        self.server_capabilities.as_ref().map(f).unwrap_or(false)
    }

    fn server_supports_text_document_open_close(capabilities: &ServerCapabilities) -> bool {
        match capabilities.text_document_sync.as_ref() {
            Some(TextDocumentSyncCapability::Kind(kind)) => *kind != TextDocumentSyncKind::NONE,
            Some(TextDocumentSyncCapability::Options(options)) => {
                options.open_close.unwrap_or(false)
            }
            None => false,
        }
    }

    fn server_supports_text_document_change(capabilities: &ServerCapabilities) -> bool {
        match capabilities.text_document_sync.as_ref() {
            Some(TextDocumentSyncCapability::Kind(kind)) => *kind != TextDocumentSyncKind::NONE,
            Some(TextDocumentSyncCapability::Options(options)) => options
                .change
                .is_some_and(|kind| kind != TextDocumentSyncKind::NONE),
            None => false,
        }
    }

    fn server_supports_text_document_save(capabilities: &ServerCapabilities) -> bool {
        match capabilities.text_document_sync.as_ref() {
            Some(TextDocumentSyncCapability::Options(options)) => match options.save.as_ref() {
                Some(TextDocumentSyncSaveOptions::Supported(supported)) => *supported,
                Some(TextDocumentSyncSaveOptions::SaveOptions(_)) => true,
                None => false,
            },
            _ => false,
        }
    }

    fn server_supports_workspace_did_create_files(capabilities: &ServerCapabilities) -> bool {
        capabilities
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.file_operations.as_ref())
            .is_some_and(|file_operations| file_operations.did_create.is_some())
    }

    fn server_supports_workspace_did_rename_files(capabilities: &ServerCapabilities) -> bool {
        capabilities
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.file_operations.as_ref())
            .is_some_and(|file_operations| file_operations.did_rename.is_some())
    }

    fn text_document_completion(
        &mut self,
        RequestParams {
            context,
            path,
            position,
            ..
        }: RequestParams,
    ) -> anyhow::Result<()> {
        if !self.has_capability(|c| c.completion_provider.is_some()) {
            return Ok(());
        }
        self.send_request::<lsp_request!("textDocument/completion")>(
            context,
            Some(path.clone()),
            CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    position: position.into(),
                    text_document: path_buf_to_text_document_identifier(path)?,
                },
                work_done_progress_params: WorkDoneProgressParams {
                    work_done_token: None,
                },
                partial_result_params: PartialResultParams {
                    partial_result_token: None,
                },
                context: None,
            },
        )
    }

    fn text_document_hover(
        &mut self,
        RequestParams {
            context,
            path,
            position,
            ..
        }: RequestParams,
    ) -> anyhow::Result<()> {
        if !self.has_capability(|c| c.hover_provider.is_some()) {
            return Ok(());
        };
        self.send_request::<lsp_request!("textDocument/hover")>(
            context,
            Some(path.clone()),
            HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    position: position.into(),
                    text_document: path_buf_to_text_document_identifier(path)?,
                },
                work_done_progress_params: WorkDoneProgressParams {
                    work_done_token: None,
                },
            },
        )
    }

    fn text_document_definition(
        &mut self,
        RequestParams {
            path,
            position,
            context,
            ..
        }: RequestParams,
    ) -> anyhow::Result<()> {
        if !self.has_capability(|c| c.definition_provider.is_some()) {
            return Ok(());
        }
        self.send_request::<lsp_request!("textDocument/definition")>(
            context,
            Some(path.clone()),
            GotoDefinitionParams {
                partial_result_params: PartialResultParams::default(),
                text_document_position_params: TextDocumentPositionParams {
                    position: position.into(),
                    text_document: path_buf_to_text_document_identifier(path)?,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            },
        )
    }

    fn text_document_references(
        &mut self,
        RequestParams {
            path,
            position,
            context,
            ..
        }: RequestParams,
        include_declaration: bool,
    ) -> anyhow::Result<()> {
        if !self.has_capability(|c| c.references_provider.is_some()) {
            return Ok(());
        }
        self.send_request::<lsp_request!("textDocument/references")>(
            context,
            Some(path.clone()),
            ReferenceParams {
                context: ReferenceContext {
                    include_declaration,
                },
                partial_result_params: PartialResultParams::default(),
                text_document_position: TextDocumentPositionParams {
                    position: position.into(),
                    text_document: path_buf_to_text_document_identifier(path)?,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            },
        )
    }

    fn text_document_declaration(&mut self, params: RequestParams) -> Result<(), anyhow::Error> {
        if !self.has_capability(|c| c.declaration_provider.is_some()) {
            return Ok(());
        }
        self.send_request::<lsp_request!("textDocument/declaration")>(
            params.context,
            Some(params.path.clone()),
            GotoDeclarationParams {
                partial_result_params: PartialResultParams::default(),
                text_document_position_params: TextDocumentPositionParams {
                    position: params.position.into(),
                    text_document: path_buf_to_text_document_identifier(params.path)?,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            },
        )
    }

    fn text_document_implementation(&mut self, params: RequestParams) -> Result<(), anyhow::Error> {
        if !self.has_capability(|c| c.implementation_provider.is_some()) {
            return Ok(());
        }
        self.send_request::<lsp_request!("textDocument/implementation")>(
            params.context,
            Some(params.path.clone()),
            GotoImplementationParams {
                partial_result_params: PartialResultParams::default(),
                text_document_position_params: TextDocumentPositionParams {
                    position: params.position.into(),
                    text_document: path_buf_to_text_document_identifier(params.path)?,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            },
        )
    }

    fn text_document_type_definition(
        &mut self,
        params: RequestParams,
    ) -> Result<(), anyhow::Error> {
        if !self.has_capability(|c| c.type_definition_provider.is_some()) {
            return Ok(());
        }
        self.send_request::<lsp_request!("textDocument/typeDefinition")>(
            params.context,
            Some(params.path.clone()),
            GotoTypeDefinitionParams {
                partial_result_params: PartialResultParams::default(),
                text_document_position_params: TextDocumentPositionParams {
                    position: params.position.into(),
                    text_document: path_buf_to_text_document_identifier(params.path)?,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            },
        )
    }

    fn text_document_prepare_rename(&mut self, params: RequestParams) -> Result<(), anyhow::Error> {
        if !self.has_capability(|c| c.rename_provider.is_some()) {
            return Ok(());
        }
        self.send_request::<lsp_request!("textDocument/prepareRename")>(
            params.context,
            Some(params.path.clone()),
            TextDocumentPositionParams {
                position: params.position.into(),
                text_document: path_buf_to_text_document_identifier(params.path)?,
            },
        )
    }

    fn text_document_rename(
        &mut self,
        params: RequestParams,
        new_name: String,
    ) -> Result<(), anyhow::Error> {
        if !self.has_capability(|c| c.rename_provider.is_some()) {
            return Ok(());
        }
        self.send_request::<lsp_request!("textDocument/rename")>(
            params.context,
            Some(params.path.clone()),
            RenameParams {
                new_name,
                text_document_position: TextDocumentPositionParams {
                    position: params.position.into(),
                    text_document: path_buf_to_text_document_identifier(params.path)?,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            },
        )
    }

    fn text_document_code_action(
        &mut self,
        params: RequestParams,
        diagnostics: Vec<Diagnostic>,
    ) -> Result<(), anyhow::Error> {
        if !self.has_capability(|c| c.code_action_provider.is_some()) {
            return Ok(());
        }
        self.send_request::<lsp_request!("textDocument/codeAction")>(
            params.context,
            Some(params.path.clone()),
            CodeActionParams {
                context: CodeActionContext {
                    diagnostics,
                    trigger_kind: None,
                    only: None,
                },
                partial_result_params: PartialResultParams::default(),
                range: Range {
                    start: params.position.into(),
                    end: params.selection_end.into(),
                },
                text_document: path_buf_to_text_document_identifier(params.path)?,
                work_done_progress_params: WorkDoneProgressParams::default(),
            },
        )
    }

    pub fn text_document_signature_help(
        &mut self,
        params: RequestParams,
    ) -> Result<(), anyhow::Error> {
        if !self.has_capability(|c| c.signature_help_provider.is_some()) {
            return Ok(());
        }
        self.send_request::<lsp_request!("textDocument/signatureHelp")>(
            params.context,
            Some(params.path.clone()),
            SignatureHelpParams {
                context: None,
                text_document_position_params: TextDocumentPositionParams {
                    position: params.position.into(),
                    text_document: path_buf_to_text_document_identifier(params.path)?,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            },
        )
    }

    fn text_document_document_symbol(
        &mut self,
        params: RequestParams,
    ) -> Result<(), anyhow::Error> {
        if !self.has_capability(|c| c.document_symbol_provider.is_some()) {
            return Ok(());
        }
        self.send_request::<lsp_request!("textDocument/documentSymbol")>(
            params.context,
            Some(params.path.clone()),
            DocumentSymbolParams {
                partial_result_params: PartialResultParams::default(),
                text_document: path_buf_to_text_document_identifier(params.path)?,
                work_done_progress_params: WorkDoneProgressParams::default(),
            },
        )
    }

    fn workspace_symbol(
        &mut self,
        context: ResponseContext,
        query: String,
    ) -> Result<(), anyhow::Error> {
        if !self.has_capability(|c| c.workspace_symbol_provider.is_some()) {
            return Ok(());
        }
        self.send_request::<lsp_request!("workspace/symbol")>(
            context,
            None,
            WorkspaceSymbolParams {
                partial_result_params: PartialResultParams::default(),
                work_done_progress_params: WorkDoneProgressParams::default(),
                query,
            },
        )
    }

    fn workspace_execute_command(
        &mut self,
        params: RequestParams,
        command: super::code_action::Command,
    ) -> Result<(), anyhow::Error> {
        if !self.has_capability(|c| c.execute_command_provider.is_some()) {
            return Ok(());
        }
        self.send_request::<lsp_request!("workspace/executeCommand")>(
            params.context,
            Some(params.path.clone()),
            ExecuteCommandParams {
                command: command.command(),
                arguments: command.arguments(),
                work_done_progress_params: WorkDoneProgressParams {
                    work_done_token: None,
                },
            },
        )
    }

    fn completion_item_resolve(
        &mut self,
        params: RequestParams,
        completion_item: lsp_types::CompletionItem,
    ) -> Result<(), anyhow::Error> {
        if !self.has_capability(|c| {
            c.completion_provider
                .as_ref()
                .map(|p| p.resolve_provider.unwrap_or(false))
                .unwrap_or(false)
        }) {
            return Ok(());
        }
        self.send_request::<lsp_request!("completionItem/resolve")>(
            params.context,
            Some(params.path),
            completion_item,
        )
    }

    fn text_document_prepare_call_hierarchy(
        &mut self,
        params: RequestParams,
        direction: CallHierarchyDirection,
    ) -> anyhow::Result<()> {
        if !self.has_capability(|c| c.call_hierarchy_provider.is_some()) {
            return Ok(());
        }
        let id = self.next_request_id;
        self.send_request::<lsp_request!("textDocument/prepareCallHierarchy")>(
            params.context,
            Some(params.path.clone()),
            CallHierarchyPrepareParams {
                text_document_position_params: TextDocumentPositionParams {
                    position: params.position.into(),
                    text_document: path_buf_to_text_document_identifier(params.path)?,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            },
        )?;
        self.pending_call_hierarchy_directions.insert(id, direction);
        Ok(())
    }

    fn call_hierarchy_incoming_calls(
        &mut self,
        context: ResponseContext,
        item: lsp_types::CallHierarchyItem,
    ) -> anyhow::Result<()> {
        self.send_request::<lsp_request!("callHierarchy/incomingCalls")>(
            context,
            None,
            CallHierarchyIncomingCallsParams {
                item,
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            },
        )
    }

    fn call_hierarchy_outgoing_calls(
        &mut self,
        context: ResponseContext,
        item: lsp_types::CallHierarchyItem,
    ) -> anyhow::Result<()> {
        self.send_request::<lsp_request!("callHierarchy/outgoingCalls")>(
            context,
            None,
            CallHierarchyOutgoingCallsParams {
                item,
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            },
        )
    }

    fn handle_from_editor(&mut self, from_editor: &FromEditor) {
        lsp_info!(
            self.lsp_command(),
            "LspServerProcess::handle_from_editor = {}",
            from_editor.variant_name()
        );
        match from_editor.clone() {
            FromEditor::TextDocumentCompletion(params) => self.text_document_completion(params),
            FromEditor::TextDocumentHover(params) => self.text_document_hover(params),
            FromEditor::TextDocumentDefinition(params) => self.text_document_definition(params),
            FromEditor::TextDocumentReferences {
                params,
                include_declaration,
            } => self.text_document_references(params, include_declaration),
            FromEditor::TextDocumentDeclaration(params) => self.text_document_declaration(params),
            FromEditor::TextDocumentImplementation(params) => {
                self.text_document_implementation(params)
            }
            FromEditor::TextDocumentTypeDefinition(params) => {
                self.text_document_type_definition(params)
            }
            FromEditor::TextDocumentRename { params, new_name } => {
                self.text_document_rename(params, new_name)
            }
            FromEditor::TextDocumentPrepareRename(params) => {
                self.text_document_prepare_rename(params)
            }
            FromEditor::TextDocumentCodeAction {
                params,
                diagnostics,
            } => self.text_document_code_action(params, diagnostics),
            FromEditor::TextDocumentDocumentSymbol(params) => {
                self.text_document_document_symbol(params)
            }

            FromEditor::WorkspaceSymbol { context, query } => self.workspace_symbol(context, query),

            FromEditor::TextDocumentDidOpen {
                file_path,
                language_id,
                version,
                content,
            } => self.text_document_did_open(file_path, language_id, version, content),
            FromEditor::TextDocumentDidChange {
                file_path,
                version,
                content,
            } => self.text_document_did_change(file_path, version, content),
            FromEditor::TextDocumentDidSave { file_path } => self.text_document_did_save(file_path),
            FromEditor::TextDocumentSignatureHelp(params) => {
                self.text_document_signature_help(params)
            }
            FromEditor::WorkspaceDidRenameFiles { old, new } => {
                self.workspace_did_rename_files(old, new)
            }
            FromEditor::WorkspaceDidCreateFiles { file_path } => {
                self.workspace_did_create_files(file_path)
            }
            FromEditor::WorkspaceExecuteCommand { params, command } => {
                self.workspace_execute_command(params, command)
            }
            FromEditor::CompletionItemResolve {
                completion_item,
                params,
            } => self.completion_item_resolve(params, *completion_item),
            FromEditor::TextDocumentPrepareCallHierarchy { params, direction } => {
                self.text_document_prepare_call_hierarchy(params, direction)
            }
        }
        .unwrap_or_else(|error| {
            lsp_info!(
                self.lsp_command(),
                "LspServerProcess::handle_from_editor | error={error:?}"
            );
        });
    }

    fn lsp_command(&self) -> String {
        self.server_config.process_command().to_string()
    }

    fn handle_progress_notification(&mut self, params: ProgressParams) {
        let token = match params.token {
            NumberOrString::Number(number) => number.to_string(),
            NumberOrString::String(string) => string,
        };
        match params.value {
            ProgressParamsValue::WorkDone(work_done_progress) => {
                self.progress_notification_manager
                    .update_progress(token, work_done_progress);
            }
        }
    }

    fn send_to_app(&self, message: AppMessage) {
        let _ = self
            .app_message_sender
            .send(message)
            .map_err(|err| log::error!("Failed to send message to app due to {err}"));
    }
}

fn path_buf_to_url(path: AbsolutePath) -> Result<Url, anyhow::Error> {
    Url::from_file_path(path.display_absolute())
        .map_err(|err| anyhow::anyhow!("Failed to convert path to URL: {err:?}"))
}

fn path_buf_to_text_document_identifier(
    path: AbsolutePath,
) -> Result<TextDocumentIdentifier, anyhow::Error> {
    Ok(TextDocumentIdentifier {
        uri: path_buf_to_url(path)?,
    })
}

/// `ErrorTracker` is created for preventing infinite error loops in LSP communication.
///
/// This exists because some LSP servers can enter states where they continuously emit
/// invalid data while keeping their pipe open.
///
/// It works by implementing a circuit breaker pattern - tracking consecutive errors
/// and allowing recovery if errors stop for a configured timeout period. If errors
/// continue beyond the maximum threshold, it breaks the connection to prevent resource waste.
struct ErrorTracker {
    consecutive_errors: Vec<String>,
    last_error_time: Instant,
    max_consecutive_errors: usize,
    error_reset_timeout: Duration,
}

impl ErrorTracker {
    const MAX_CONSECUTIVE_ERRORS: usize = 5;
    const ERROR_RESET_TIMEOUT: Duration = Duration::from_secs(30);

    fn new() -> Self {
        Self {
            consecutive_errors: Vec::new(),
            last_error_time: Instant::now(),
            max_consecutive_errors: Self::MAX_CONSECUTIVE_ERRORS,
            error_reset_timeout: Self::ERROR_RESET_TIMEOUT,
        }
    }

    /// Returns true if should continue, false if should break
    fn handle_error(
        &mut self,
        error: anyhow::Error,
        sender: &Sender<LspServerProcessMessage>,
    ) -> bool {
        let is_unexpected_eof = error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::UnexpectedEof);
        if self.last_error_time.elapsed() > self.error_reset_timeout {
            self.consecutive_errors = Vec::new();
        }

        self.consecutive_errors.push(format!("Error: {error}"));
        self.last_error_time = Instant::now();

        log::warn!(
            "LspServerProcess::listen::read_response error (attempt {}/{}): {}",
            self.consecutive_errors.len(),
            self.max_consecutive_errors,
            error
        );
        if is_unexpected_eof || self.consecutive_errors.len() >= self.max_consecutive_errors {
            let _ = sender.send(LspServerProcessMessage::Shutdown);
            return false;
        }

        thread::sleep(Duration::from_millis(100));
        true
    }

    fn handle_success(&mut self) {
        self.consecutive_errors.clear();
    }
}

#[cfg(test)]
mod test_lsp_server_process {
    use super::*;
    use std::process::Command;
    use std::sync::mpsc;

    #[cfg(unix)]
    #[test]
    fn shutdown_before_initialization_kills_the_process_tree() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let child_pid_path = tempdir.path().join("child-pid");
        let script = format!(
            "sleep 30 & child=$!; printf '%s' \"$child\" > {}; wait",
            child_pid_path.display()
        );
        let command =
            shared::process_command::ProcessCommand::new("sh", &["-c".to_string(), script]);
        let child = command.spawn_in_directory_in_new_process_group(tempdir.path())?;
        let process_group = ProcessGroup(child.id());
        let (sender, receiver) = mpsc::channel();
        let channel = LspServerProcessChannel {
            language: Language::default(),
            server_config: LspServerConfig::new(
                "test",
                shared::language::Command::new("test-lsp", &[]),
            ),
            sender,
            is_initialized: false,
            child: Some(child),
            process_group,
        };

        let child_pid = (0..100)
            .find_map(|_| {
                let pid = std::fs::read_to_string(&child_pid_path)
                    .ok()
                    .and_then(|pid| pid.parse::<libc::pid_t>().ok());
                if pid.is_none() {
                    thread::sleep(Duration::from_millis(10));
                }
                pid
            })
            .ok_or_else(|| anyhow::anyhow!("shell did not start its child"))?;

        channel.request_shutdown()?;
        assert!(matches!(
            receiver.recv()?,
            LspServerProcessMessage::Shutdown
        ));
        channel.wait_for_exit_until(Instant::now());

        let child_was_killed = (0..100).any(|_| {
            let result = unsafe { libc::kill(child_pid, 0) };
            if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                true
            } else {
                thread::sleep(Duration::from_millis(10));
                false
            }
        });
        assert!(child_was_killed, "LSP descendant survived shutdown");
        Ok(())
    }

    #[test]
    fn initialized_shutdown_sends_shutdown_and_exit() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let messages_path = tempdir.path().join("messages");
        let output = std::fs::File::create(&messages_path)?;
        let mut child = Command::new("sh")
            .args(["-c", "cat"])
            .stdin(std::process::Stdio::piped())
            .stdout(output)
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (app_sender, _app_receiver) = crossbeam_channel::unbounded();
        let (sender, _receiver) = mpsc::channel();
        let mut lsp_process = LspServerProcess {
            language: Language::default(),
            server_config: LspServerConfig::new(
                "test",
                shared::language::Command::new("test-lsp", &[]),
            ),
            stdin,
            stdout: None,
            stderr: Some(stderr),
            server_capabilities: None,
            current_working_directory: std::env::current_dir()?.try_into()?,
            next_request_id: 0,
            pending_response_requests: HashMap::new(),
            pending_call_hierarchy_directions: HashMap::new(),
            app_message_sender: app_sender,
            sender,
            progress_notification_manager: ProgressNotificationManager::new(
                "nothing".to_string(),
                Callback::new(Arc::new(|_| {})),
            ),
            is_initialized: true,
        };

        lsp_process.shutdown()?;
        drop(lsp_process);
        child.wait()?;
        let messages = std::fs::read_to_string(messages_path)?;

        assert!(messages.contains("\"method\":\"shutdown\""));
        assert!(messages.contains("\"method\":\"exit\""));
        Ok(())
    }

    #[test]
    fn text_document_lifecycle_notifications_require_server_sync_capabilities() {
        let mut capabilities = ServerCapabilities::default();

        assert!(!LspServerProcess::server_supports_text_document_open_close(
            &capabilities
        ));
        assert!(!LspServerProcess::server_supports_text_document_change(
            &capabilities
        ));
        assert!(!LspServerProcess::server_supports_text_document_save(
            &capabilities
        ));

        capabilities.text_document_sync =
            Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL));
        assert!(LspServerProcess::server_supports_text_document_open_close(
            &capabilities
        ));
        assert!(LspServerProcess::server_supports_text_document_change(
            &capabilities
        ));
        assert!(!LspServerProcess::server_supports_text_document_save(
            &capabilities
        ));

        capabilities.text_document_sync = Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::FULL),
                save: Some(TextDocumentSyncSaveOptions::Supported(true)),
                ..TextDocumentSyncOptions::default()
            },
        ));
        assert!(LspServerProcess::server_supports_text_document_open_close(
            &capabilities
        ));
        assert!(LspServerProcess::server_supports_text_document_change(
            &capabilities
        ));
        assert!(LspServerProcess::server_supports_text_document_save(
            &capabilities
        ));
    }

    #[test]
    fn workspace_file_notifications_require_server_file_operation_capabilities() {
        let mut capabilities = ServerCapabilities::default();

        assert!(!LspServerProcess::server_supports_workspace_did_create_files(&capabilities));
        assert!(!LspServerProcess::server_supports_workspace_did_rename_files(&capabilities));

        capabilities.workspace = Some(WorkspaceServerCapabilities {
            file_operations: Some(WorkspaceFileOperationsServerCapabilities {
                did_create: Some(FileOperationRegistrationOptions { filters: vec![] }),
                did_rename: Some(FileOperationRegistrationOptions { filters: vec![] }),
                ..WorkspaceFileOperationsServerCapabilities::default()
            }),
            ..WorkspaceServerCapabilities::default()
        });

        assert!(LspServerProcess::server_supports_workspace_did_create_files(&capabilities));
        assert!(LspServerProcess::server_supports_workspace_did_rename_files(&capabilities));
    }

    #[test]
    fn full_document_diagnostic_report_returns_diagnostics() {
        let diagnostic = lsp_types::Diagnostic::new_simple(
            lsp_types::Range::new(
                lsp_types::Position::new(0, 0),
                lsp_types::Position::new(0, 1),
            ),
            "hello".to_string(),
        );
        let result = DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(
            RelatedFullDocumentDiagnosticReport {
                related_documents: None,
                full_document_diagnostic_report: FullDocumentDiagnosticReport {
                    result_id: None,
                    items: vec![diagnostic.clone()],
                },
            },
        ));

        assert_eq!(
            diagnostics_from_document_diagnostic(result),
            Some(vec![diagnostic])
        );
    }

    #[test]
    fn workspace_configuration_includes_vue_eslint_validation() {
        let root: AbsolutePath = std::env::current_dir().unwrap().try_into().unwrap();
        let response = workspace_configuration_response(
            ConfigurationParams {
                items: vec![ConfigurationItem {
                    scope_uri: None,
                    section: Some("eslint".to_string()),
                }],
            },
            &root,
        );
        let configs = response.as_array().unwrap();
        assert!(configs[0]["validate"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("vue")));
    }

    #[test]
    fn workspace_configuration_includes_vue_typescript_plugin_for_vtsls() {
        let root: AbsolutePath = std::env::current_dir().unwrap().try_into().unwrap();
        let response = workspace_configuration_response(
            ConfigurationParams {
                items: vec![ConfigurationItem {
                    scope_uri: None,
                    section: Some("".to_string()),
                }],
            },
            &root,
        );
        let configs = response.as_array().unwrap();
        assert_eq!(
            configs[0]["typescript.tsdk"],
            format!(
                "{}/node_modules/typescript/lib",
                std::env::current_dir().unwrap().display()
            )
        );
        assert_eq!(
            configs[0]["vtsls"]["tsserver"]["globalPlugins"][0]["name"],
            "@vue/typescript-plugin"
        );
    }

    #[test]
    fn lsp_error_hint_mentions_vue_typescript_plugin() {
        let hint = hint_for_lsp_error(
            "Request textDocument/definition failed with message: Cannot find provider for definition, the feature is possibly not supported by the current TypeScript version or disabled by settings.",
        )
        .unwrap();

        assert!(hint.contains("@vue/typescript-plugin"));
    }

    #[test]
    fn lsp_error_hint_mentions_eslint_resolution() {
        let hint = hint_for_lsp_error(
            "Request textDocument/diagnostic failed with message: The \"path\" argument must be of type string. Received undefined",
        )
        .unwrap();

        assert!(hint.contains("ESLint"));
    }

    #[test]
    fn lsp_should_shutdown_after_too_many_consecutive_errors() -> anyhow::Result<()> {
        let (app_sender, app_receiver) = crossbeam_channel::unbounded();
        let (sender, receiver) = mpsc::channel();

        // Create a process that will output invalid LSP data quickly
        let mut process = Command::new("sh")
            .args(["-c", "for i in {1..10}; do echo 'invalid data'; done"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;

        let stdin = process.stdin.take().unwrap();
        let stdout = process.stdout.take().unwrap();
        let stderr = process.stderr.take().unwrap();

        let lsp_process = LspServerProcess {
            language: Language::default(),
            server_config: LspServerConfig::new(
                "test",
                shared::language::Command::new("test-lsp", &[]),
            ),
            stdin,
            stdout: Some(stdout),
            stderr: Some(stderr),
            server_capabilities: None,
            current_working_directory: std::env::current_dir()?.try_into()?,
            next_request_id: 0,
            pending_response_requests: HashMap::new(),
            pending_call_hierarchy_directions: HashMap::new(),
            app_message_sender: app_sender.clone(),
            sender,
            progress_notification_manager: ProgressNotificationManager::new(
                "nothing".to_string(),
                Callback::new(Arc::new(|_| {})),
            ),
            is_initialized: false,
        };

        // Start listening in a separate thread
        let handle = lsp_process.listen(receiver, app_sender)?;

        // We expect an error message after max consecutive errors
        match app_receiver.recv_timeout(Duration::from_secs(1)) {
            Ok(AppMessage::LspNotification(notification)) => {
                if let LspNotification::Error(msg) = *notification {
                    assert!(msg.contains("Too many consecutive errors"));
                }
            }
            other => panic!("Expected error notification, got: {other:?}"),
        }

        // Verify the thread has actually finished by waiting a short time
        // If join returns Ok, it means the thread completed (loop was escaped)
        // If it's still running, join_timeout would return Err
        thread::sleep(Duration::from_secs(1));
        assert!(
            handle.is_finished(),
            "Listen loop didn't escape after max errors"
        );
        Ok(())
    }
}
