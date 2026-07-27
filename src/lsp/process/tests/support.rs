use super::super::*;
use std::process::Command;
use std::sync::mpsc;

pub(super) type ServerRequestProcess = (
    LspServerProcess,
    process::Child,
    crossbeam_channel::Receiver<AppMessage>,
    Sender<LspServerProcessMessage>,
    Receiver<LspServerProcessMessage>,
    tempfile::TempDir,
    std::path::PathBuf,
);

pub(super) fn process_for_server_requests() -> anyhow::Result<ServerRequestProcess> {
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
    let (app_sender, app_receiver) = crossbeam_channel::unbounded();
    let (sender, receiver) = mpsc::channel();
    let process = LspServerProcess {
        language: Language::default(),
        server_config: LspServerConfig::new(
            "test",
            shared::language::Command::new("test-lsp", &[]),
        ),
        stdin,
        stdout: None,
        stderr: Some(stderr),
        server_capabilities: None,
        synchronized_documents: HashMap::new(),
        current_working_directory: std::env::current_dir()?.try_into()?,
        next_request_id: 0,
        pending_response_requests: HashMap::new(),
        pending_call_hierarchy_directions: HashMap::new(),
        app_message_sender: app_sender,
        sender: sender.clone(),
        progress_notification_manager: ProgressNotificationManager::new(
            "nothing".to_string(),
            Callback::new(Arc::new(|_| {})),
        ),
        is_initialized: true,
    };

    Ok((
        process,
        child,
        app_receiver,
        sender,
        receiver,
        tempdir,
        messages_path,
    ))
}

pub(super) fn read_json_rpc_messages(
    path: &std::path::Path,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let mut reader = BufReader::new(std::fs::File::open(path)?);
    let mut messages = Vec::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        let content_length = header
            .strip_prefix("Content-Length: ")
            .and_then(|header| header.trim().parse::<usize>().ok())
            .ok_or_else(|| anyhow::anyhow!("Invalid LSP header: {header:?}"))?;
        loop {
            header.clear();
            reader.read_line(&mut header)?;
            if header == "\r\n" {
                break;
            }
        }
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body)?;
        messages.push(serde_json::from_slice(&body)?);
    }
    Ok(messages)
}

pub(super) fn sync_capabilities(kind: TextDocumentSyncKind) -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(kind),
                ..TextDocumentSyncOptions::default()
            },
        )),
        ..ServerCapabilities::default()
    }
}
