use anyhow::Context;
use std::{
    collections::HashMap,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct ProcessCommand {
    command: String,
    args: Vec<String>,
    environment: HashMap<String, String>,
}

pub enum SpawnCommandResult {
    CommandNotFound { command_name: String },
    Spawned(anyhow::Result<std::process::Child>),
}

#[derive(Debug)]
pub enum SpawnCommandError {
    CommandNotFound { command: String },
}

impl std::fmt::Display for SpawnCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:#?}")
    }
}

impl std::error::Error for SpawnCommandError {}

impl ProcessCommand {
    pub fn new(command: &str, args: &[String]) -> Self {
        Self {
            command: command.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            environment: HashMap::new(),
        }
    }

    pub fn with_environment(
        command: &str,
        args: &[String],
        environment: &HashMap<String, String>,
    ) -> Self {
        Self {
            command: command.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            environment: environment.clone(),
        }
    }

    pub fn command(&self) -> &str {
        &self.command
    }

    /// Returns `true` if `self.command` can be located on `$PATH`.
    pub fn is_command_found(&self) -> bool {
        which::which(&self.command).is_ok()
    }

    pub fn spawn(&self) -> anyhow::Result<std::process::Child> {
        self.spawn_inner(None, false)
    }

    pub fn spawn_in_directory(&self, directory: &Path) -> anyhow::Result<std::process::Child> {
        self.spawn_inner(Some(directory), false)
    }

    pub fn spawn_in_directory_in_new_process_group(
        &self,
        directory: &Path,
    ) -> anyhow::Result<std::process::Child> {
        self.spawn_inner(Some(directory), true)
    }

    fn spawn_inner(
        &self,
        directory: Option<&Path>,
        new_process_group: bool,
    ) -> anyhow::Result<std::process::Child> {
        log::info!(
            "ProcessCommand::spawn {:?} {:?} in {:?}",
            self.command,
            self.args,
            directory
        );
        let Some(command) = self.resolve_command(directory) else {
            log::warn!("ProcessCommand::spawn: Failed to locate {:?}", self.command);
            return Err(SpawnCommandError::CommandNotFound {
                command: self.command.clone(),
            }
            .into());
        };

        let mut command = std::process::Command::new(command);
        if let Some(directory) = directory {
            command.current_dir(directory);
        }
        command
            .args(&self.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .envs(&self.environment);
        #[cfg(unix)]
        if new_process_group {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(not(unix))]
        let _ = new_process_group;
        command.spawn().map_err(|e| {
            anyhow::anyhow!(
                "Failed to spawn the command: {:?} with error: {:?}",
                self,
                e
            )
        })
    }

    fn resolve_command(&self, directory: Option<&Path>) -> Option<PathBuf> {
        let command_path = Path::new(&self.command);
        let command_has_separator = self.command.contains(std::path::MAIN_SEPARATOR)
            || self.command.contains('/')
            || self.command.contains('\\');
        if command_path.is_absolute() || command_has_separator {
            return Some(
                directory
                    .filter(|_| command_path.is_relative())
                    .map(|directory| directory.join(command_path))
                    .unwrap_or_else(|| command_path.to_path_buf()),
            );
        }

        directory
            .map(|directory| {
                directory
                    .join("node_modules")
                    .join(".bin")
                    .join(&self.command)
            })
            .filter(|path| path.exists())
            .or_else(|| which::which(&self.command).ok())
    }

    pub fn run_with_input(&self, input: &str) -> anyhow::Result<String> {
        let mut child = self.spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(input.as_bytes())
                .context("Failed to write to stdin")?;
        } else {
            return Err(anyhow::anyhow!("Failed to open stdin"));
        }

        let mut output = String::new();
        if let Some(mut stdout) = child.stdout.take() {
            stdout
                .read_to_string(&mut output)
                .context("Failed to read from stdout")?;
        } else {
            return Err(anyhow::anyhow!("Failed to open stdout"));
        }

        let status = child.wait().context("Failed to wait on child process")?;

        if !status.success() {
            let stderr = child
                .stderr
                .take()
                .map(|mut stderr| -> anyhow::Result<_> {
                    let mut output = String::new();
                    stderr.read_to_string(&mut output)?;
                    Ok(output)
                })
                .unwrap_or(Ok("[No stderr]".to_string()))
                .unwrap_or("[Failed to obtain stderr]".to_string());
            return Err(anyhow::anyhow!(
                "Command failed with exit code: {}\n\nSTDERR =\n\n{}\n\nSTDOUT =\n\n{}",
                status
                    .code()
                    .map(|code| code.to_string())
                    .unwrap_or("[Process terminated by signal]".to_string()),
                stderr,
                output,
            ));
        }

        Ok(output)
    }

    pub fn run(&self) -> Result<String, anyhow::Error> {
        self.run_with_input("")
    }
}

impl std::fmt::Display for ProcessCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.args.is_empty() {
            write!(f, "{}", self.command)
        } else {
            write!(f, "{} {}", self.command, self.args.join(" "))
        }
    }
}

#[cfg(test)]
mod test_process_command {
    use std::{
        fs,
        io::Read,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
    };

    use super::ProcessCommand;

    fn create_executable(path: &Path, content: &str) {
        fs::write(path, content).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[test]
    fn failed_command_includes_exit_code_and_stderr() {
        let err = ProcessCommand::new("bash", ["-c".to_string(), "yo".to_string()].as_ref())
            .run_with_input("hello")
            .unwrap_err();
        let error = err.to_string();
        assert!(error.contains("Command failed with exit code: 127"));
        assert!(error.contains("bash: line 1: yo: command not found"));
    }

    #[test]
    fn spawn_in_directory_prefers_workspace_node_modules_bin() {
        let tempdir = tempfile::tempdir().unwrap();
        let bin_dir = tempdir.path().join("node_modules").join(".bin");
        fs::create_dir_all(&bin_dir).unwrap();
        create_executable(
            &bin_dir.join("hello-lsp"),
            "#!/usr/bin/env bash\nprintf local-node-modules",
        );

        let command = ProcessCommand::new("hello-lsp", &[]);
        let mut child = command.spawn_in_directory(tempdir.path()).unwrap();
        let mut stdout = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        child.wait().unwrap();

        assert_eq!(stdout, "local-node-modules");
    }

    #[test]
    fn relative_command_is_resolved_from_directory() {
        let tempdir = tempfile::tempdir().unwrap();
        let script = PathBuf::from("scripts").join("hello-lsp");
        fs::create_dir_all(tempdir.path().join("scripts")).unwrap();
        create_executable(
            &tempdir.path().join(&script),
            "#!/usr/bin/env bash\nprintf relative-command",
        );

        let command = ProcessCommand::new(script.to_str().unwrap(), &[]);
        let mut child = command.spawn_in_directory(tempdir.path()).unwrap();
        let mut stdout = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        child.wait().unwrap();

        assert_eq!(stdout, "relative-command");
    }
}
