use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ExitStatus},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use crate::{
    language::ProcessCommand,
    language_servers::{
        package_workspace,
        workspace_root::{RootFallback, WorkspaceRootPolicy},
    },
};

const FORMATTER_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Formatter {
    process_command: ProcessCommand,
    timeout: Duration,
}

impl From<ProcessCommand> for Formatter {
    fn from(value: ProcessCommand) -> Self {
        Self {
            process_command: value,
            timeout: FORMATTER_TIMEOUT,
        }
    }
}

impl Formatter {
    fn working_directory(&self, file_path: &Path) -> Option<PathBuf> {
        let executable = Path::new(self.process_command.command())
            .file_stem()
            .and_then(|name| name.to_str());
        if !matches!(executable, Some("prettier" | "prettierd")) {
            return None;
        }

        let parent = file_path.parent().filter(|path| path.is_absolute())?;
        // Prettier resolves bare plugin names from CWD, not the config path.
        // Search beyond Ki's CWD so opening an app directly still finds its workspace.
        let boundary = parent.ancestors().last()?;
        Some(
            WorkspaceRootPolicy {
                marker_groups: &[&[
                    package_workspace::WORKSPACE_MARKERS,
                    package_workspace::PROJECT_MARKERS,
                ]],
                fallback: RootFallback::FileParent,
                canonicalize: false,
            }
            .resolve(parent, boundary),
        )
    }

    pub fn format(&self, content: &str, file_path: &Path) -> anyhow::Result<String> {
        // Run the command with the args,
        // pass in the content using stdin,
        // get the output from the stdout

        let working_directory = self.working_directory(file_path);
        let configured_command = Path::new(self.process_command.command());
        let resolved_command = match working_directory.as_deref() {
            // Explicit relative commands remain relative to Ki, not the buffer's package.
            Some(_)
                if configured_command.is_relative()
                    && configured_command.components().count() > 1 =>
            {
                Some(std::path::absolute(configured_command)?)
            }
            Some(directory) if configured_command.components().count() == 1 => directory
                .ancestors()
                .map(|parent| parent.join("node_modules/.bin").join(configured_command))
                .find(|path| path.is_file()),
            _ => None,
        };
        let command_name = resolved_command
            .as_deref()
            .unwrap_or(configured_command)
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Formatter command path is not valid UTF-8"))?;
        let file_path = file_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Formatter file path is not valid UTF-8"))?;
        let arguments = self
            .process_command
            .arguments()
            .iter()
            .map(|argument| argument.replace("{file_path}", file_path))
            .collect::<Vec<_>>();
        let command = ProcessCommand::with_environment(
            command_name,
            &arguments,
            self.process_command.environment(),
        );
        let mut child = match working_directory {
            Some(directory) => command.spawn_in_directory(&directory),
            None => command.spawn(),
        }?;
        let deadline = Instant::now() + self.timeout;

        let stdin = child.stdin.take().ok_or_else(|| {
            anyhow::anyhow!(
                "Failed to open stdin for the command: {:?}",
                self.process_command
            )
        })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to open stdout for the command"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to open stderr for the command"))?;

        // Every pipe is handled on its own thread, so that a formatter that stops
        // reading its input or writing its output can never block us past the deadline.
        let stdin = spawn_in_background({
            let content = content.to_string();
            move || {
                let mut stdin = stdin;
                stdin.write_all(content.as_bytes())
            }
        });
        let stdout = spawn_in_background(move || read_to_end(stdout));
        let stderr = spawn_in_background(move || read_to_end(stderr));

        let Some(status) = wait_until(&mut child, deadline)? else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow::anyhow!(
                "The formatter `{}` did not finish within {:?} and was killed",
                self.process_command,
                self.timeout
            ));
        };

        // The pipes may still be held open by processes spawned by the formatter,
        // so the deadline applies to them too.
        receive_until(&stdin, deadline)?;
        let stdout = receive_until(&stdout, deadline)?;
        let stderr = receive_until(&stderr, deadline)?;

        if !status.success() {
            let stdout =
                String::from_utf8(stdout.clone()).unwrap_or_else(|_| format!("{stdout:?}"));
            let stderr =
                String::from_utf8(stderr.clone()).unwrap_or_else(|_| format!("{stderr:?}"));
            Err(anyhow::anyhow!(
                "[[STDERR]]:\n{stderr}\n\n[[STDOUT]]:\n{stdout}"
            ))
        } else {
            Ok(String::from_utf8(stdout)?)
        }
    }
}

fn read_to_end(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut buffer = Vec::new();
    reader.read_to_end(&mut buffer).map(|_| buffer)
}

fn spawn_in_background<T: Send + 'static>(
    f: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Receiver<std::io::Result<T>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(f());
    });
    receiver
}

fn receive_until<T>(
    receiver: &Receiver<std::io::Result<T>>,
    deadline: Instant,
) -> anyhow::Result<T> {
    receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| anyhow::anyhow!("Timed out communicating with the formatter"))?
        .map_err(Into::into)
}

/// Returns `None` if the child is still running once `deadline` is reached.
fn wait_until(child: &mut Child, deadline: Instant) -> anyhow::Result<Option<ExitStatus>> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod test_formatter {
    use std::time::{Duration, Instant};

    use super::Formatter;
    use crate::language::ProcessCommand;

    fn formatter(script: &str, timeout: Duration) -> Formatter {
        Formatter {
            process_command: ProcessCommand::new("sh", &["-c".to_string(), script.to_string()]),
            timeout,
        }
    }

    #[test]
    fn formats_through_stdin_and_stdout() {
        let output = formatter("tr a-z A-Z", Duration::from_secs(10))
            .format("hello", std::path::Path::new("input.txt"))
            .unwrap();
        assert_eq!(output, "HELLO");
    }

    #[test]
    fn handles_output_larger_than_pipe_buffer() {
        let content = "x".repeat(1024 * 1024);
        let output = formatter("cat", Duration::from_secs(10))
            .format(&content, std::path::Path::new("input.txt"))
            .unwrap();
        assert_eq!(output, content);
    }

    #[test]
    fn hanging_formatter_is_killed_after_timeout() {
        let started_at = Instant::now();
        let error = formatter("cat > /dev/null; sleep 60", Duration::from_millis(200))
            .format("hello", std::path::Path::new("input.txt"))
            .unwrap_err();
        assert!(started_at.elapsed() < Duration::from_secs(5));
        assert!(
            error.to_string().contains("did not finish within"),
            "{error}"
        );
    }

    #[test]
    fn formatter_ignoring_its_stdin_is_killed_after_timeout() {
        let content = "x".repeat(1024 * 1024);
        let error = formatter("sleep 60", Duration::from_millis(200))
            .format(&content, std::path::Path::new("input.txt"))
            .unwrap_err();
        assert!(
            error.to_string().contains("did not finish within"),
            "{error}"
        );
    }

    #[test]
    fn failing_formatter_reports_stderr() {
        let error = formatter("echo oops >&2; exit 1", Duration::from_secs(10))
            .format("hello", std::path::Path::new("input.txt"))
            .unwrap_err();
        assert!(error.to_string().contains("oops"), "{error}");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use super::*;

    #[test]
    fn prettier_uses_nearest_package_and_workspace_executable() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let frontend = dir.path().canonicalize()?.join("frontends");
        let app = frontend.join("apps/app with spaces");
        let bin = frontend.join("node_modules/.bin");
        fs::create_dir_all(&app)?;
        fs::create_dir_all(&bin)?;
        fs::write(frontend.join("pnpm-workspace.yaml"), "packages: [apps/*]\n")?;
        fs::write(app.join("package.json"), "{}")?;
        let file_path = app.join("Component.vue");
        let original_directory = std::env::current_dir()?;

        for executable in ["prettier", "prettierd"] {
            let command_path = bin.join(executable);
            fs::write(
                &command_path,
                "#!/bin/sh\npwd -P\nprintf '%s\\n' \"$1\"\ncat\n",
            )?;
            fs::set_permissions(&command_path, fs::Permissions::from_mode(0o755))?;

            // Both PATH-style and absolute executable names use the buffer's workspace.
            for command in [executable, command_path.to_str().unwrap()] {
                let formatter =
                    Formatter::from(ProcessCommand::new(command, &["{file_path}".to_string()]));
                assert_eq!(
                    formatter.format("input\n", &file_path)?,
                    format!("{}\n{}\ninput\n", app.display(), file_path.display())
                );
            }
        }

        assert_eq!(std::env::current_dir()?, original_directory);
        Ok(())
    }

    #[test]
    fn prettier_preserves_explicit_relative_executable() -> anyhow::Result<()> {
        let dir = tempfile::tempdir_in(std::env::current_dir()?)?;
        let tools = dir.path().join("tools");
        let package = dir.path().join("package");
        fs::create_dir_all(&tools)?;
        fs::create_dir_all(&package)?;
        fs::write(package.join("package.json"), "{}")?;
        let executable = tools.join("prettierd");
        fs::write(&executable, "#!/bin/sh\ncat\n")?;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
        let relative_command = executable.strip_prefix(std::env::current_dir()?)?;
        let formatter =
            Formatter::from(ProcessCommand::new(relative_command.to_str().unwrap(), &[]));
        assert_eq!(
            formatter.format("input\n", &package.join("Component.vue"))?,
            "input\n"
        );
        Ok(())
    }

    #[test]
    fn prettier_falls_back_to_package_then_file_directory() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let package = dir.path().join("package");
        let components = package.join("components");
        fs::create_dir_all(&components)?;
        let file_path = components.join("Component.vue");
        let formatter = Formatter::from(ProcessCommand::new("prettierd", &[]));

        assert_eq!(formatter.working_directory(&file_path), Some(components));
        fs::write(package.join("package.json"), "{}")?;
        assert_eq!(formatter.working_directory(&file_path), Some(package));
        Ok(())
    }

    #[test]
    fn other_formatters_keep_ki_working_directory() -> anyhow::Result<()> {
        let formatter = Formatter::from(ProcessCommand::new(
            "sh",
            &["-c".to_string(), "pwd -P; cat".to_string()],
        ));
        assert_eq!(
            formatter.format("input\n", Path::new("/project/Component.vue"))?,
            format!(
                "{}\ninput\n",
                std::env::current_dir()?.canonicalize()?.display()
            )
        );
        Ok(())
    }

    #[test]
    fn formatter_substitutes_file_path_without_shell_expansion() {
        let formatter = Formatter::from(ProcessCommand::with_environment(
            "sh",
            &[
                "-c".to_string(),
                "printf '%s\\n' \"$#\" \"$1\" \"$2\" \"$3\" \"$FORMATTER_TEST\"; cat".to_string(),
                "formatter".to_string(),
                "{file_path}".to_string(),
                "--stdin-filepath={file_path}".to_string(),
                "literal".to_string(),
            ],
            &[("FORMATTER_TEST".to_string(), "preserved".to_string())].into(),
        ));
        let path = Path::new("/project with spaces/$(exit 1)/Component.vue");
        assert_eq!(
            formatter.format("input\n", path).unwrap(),
            format!(
                "3\n{}\n--stdin-filepath={}\nliteral\npreserved\ninput\n",
                path.display(),
                path.display()
            )
        );
    }

    #[test]
    fn formatter_without_placeholder_still_receives_stdin() {
        let formatter = Formatter::from(ProcessCommand::new("cat", &[]));
        assert_eq!(
            formatter
                .format("\tinput\n", Path::new("/project/main.rs"))
                .unwrap(),
            "\tinput\n"
        );
    }
}
