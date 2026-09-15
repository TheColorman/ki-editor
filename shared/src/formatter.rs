use std::{io::Write, path::Path};

use crate::language::ProcessCommand;

pub struct Formatter {
    process_command: ProcessCommand,
}

impl From<ProcessCommand> for Formatter {
    fn from(value: ProcessCommand) -> Self {
        Self {
            process_command: value,
        }
    }
}

impl Formatter {
    pub fn format(&self, content: &str, file_path: &Path) -> anyhow::Result<String> {
        // Run the command with the args,
        // pass in the content using stdin,
        // get the output from the stdout

        let file_path = file_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Formatter file path is not valid UTF-8"))?;
        let arguments = self
            .process_command
            .arguments()
            .iter()
            .map(|argument| argument.replace("{file_path}", file_path))
            .collect::<Vec<_>>();
        let mut child = ProcessCommand::with_environment(
            self.process_command.command(),
            &arguments,
            self.process_command.environment(),
        )
        .spawn()?;

        let stdin = child.stdin.as_mut().ok_or_else(|| {
            anyhow::anyhow!(
                "Failed to open stdin for the command: {:?}",
                self.process_command
            )
        })?;

        stdin.write_all(content.as_bytes())?;

        // Read from stdout
        let output = child.wait_with_output()?;

        if !output.status.success() {
            let stdout = String::from_utf8(output.stdout.clone())
                .unwrap_or_else(|_| format!("{:?}", output.stdout));
            let stderr = String::from_utf8(output.stderr.clone())
                .unwrap_or_else(|_| format!("{:?}", output.stderr));
            Err(anyhow::anyhow!(
                "[[STDERR]]:\n{stderr}\n\n[[STDOUT]]:\n{stdout}"
            ))
        } else {
            Ok(String::from_utf8(output.stdout)?)
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

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
