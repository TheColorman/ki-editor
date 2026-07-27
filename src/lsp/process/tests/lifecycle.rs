use super::super::*;
use std::process::Command;
use std::sync::mpsc;

#[cfg(unix)]
#[test]
fn shutdown_before_initialization_kills_the_process_tree() -> anyhow::Result<()> {
    let tempdir = tempfile::tempdir()?;
    let child_pid_path = tempdir.path().join("child-pid");
    let script = format!(
        "sleep 30 & child=$!; printf '%s' \"$child\" > {}",
        child_pid_path.display()
    );
    let command = shared::process_command::ProcessCommand::new("sh", &["-c".to_string(), script]);
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
        alive: Arc::new(AtomicBool::new(true)),
        _workspace_data_lease: None,
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

#[cfg(unix)]
#[test]
fn process_channel_reports_exited_child() -> anyhow::Result<()> {
    let child = Command::new("sh")
        .args(["-c", "exit 0"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let process_group = ProcessGroup(child.id());
    let (sender, _receiver) = mpsc::channel();
    let mut channel = LspServerProcessChannel {
        language: Language::default(),
        server_config: LspServerConfig::new(
            "test",
            shared::language::Command::new("test-lsp", &[]),
        ),
        sender,
        is_initialized: false,
        child: Some(child),
        process_group,
        alive: Arc::new(AtomicBool::new(true)),
        _workspace_data_lease: None,
    };

    for _ in 0..100 {
        if !channel.is_running() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }

    anyhow::bail!("child process did not exit")
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
        synchronized_documents: HashMap::new(),
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
    lsp_process.handle_reply(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 0,
        "result": null
    }))?;
    assert!(matches!(_receiver.recv()?, LspServerProcessMessage::Exit));
    drop(lsp_process);
    child.wait()?;
    let messages = std::fs::read_to_string(messages_path)?;

    assert!(messages.contains("\"method\":\"shutdown\""));
    assert!(messages.contains("\"method\":\"exit\""));
    Ok(())
}

#[test]
fn broken_pipe_is_a_transport_error() {
    let error = anyhow::Error::from(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "server closed stdin",
    ));
    assert!(LspServerProcess::is_transport_error(&error));
    assert!(!LspServerProcess::is_transport_error(&anyhow::anyhow!(
        "invalid request"
    )));
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
        synchronized_documents: HashMap::new(),
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
