use itertools::Itertools;

use crate::{app::Dispatch, components::dropdown_sync::DropdownItem};

use super::workspace_edit::WorkspaceEdit;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Refer https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#codeAction
pub struct CodeAction {
    pub title: String,
    pub kind: Option<String>,
    pub edit: Option<WorkspaceEdit>,
    pub command: Option<Command>,
}

#[derive(Debug, Clone)]
pub struct Command(lsp_types::Command);
impl Command {
    pub fn arguments(&self) -> Vec<serde_json::Value> {
        self.0.arguments.clone().unwrap_or_default()
    }

    pub fn command(&self) -> String {
        self.0.command.clone()
    }
}

impl From<lsp_types::Command> for Command {
    fn from(value: lsp_types::Command) -> Self {
        Self(value)
    }
}

impl From<lsp_types::Command> for CodeAction {
    fn from(command: lsp_types::Command) -> Self {
        Self {
            title: command.title.clone(),
            kind: None,
            edit: None,
            command: Some(command.into()),
        }
    }
}

impl PartialEq for Command {
    fn eq(&self, other: &Self) -> bool {
        self.0.command.eq(&other.0.command)
    }
}

impl Eq for Command {}

impl From<CodeAction> for DropdownItem {
    fn from(value: CodeAction) -> DropdownItem {
        DropdownItem::new(value.title)
            .set_group(Some(
                value
                    .kind
                    .and_then(|kind| if kind.is_empty() { None } else { Some(kind) })
                    .unwrap_or("Misc.".to_string()),
            ))
            .set_dispatches(
                value
                    .edit
                    .map(Dispatch::ApplyWorkspaceEdit)
                    .into_iter()
                    // A command this code action executes. If a code action
                    // provides an edit and a command, first the edit is
                    // executed and then the command.
                    // Refer https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#codeAction
                    .chain(
                        value
                            .command
                            .map(|command| Dispatch::LspExecuteCommand { command }),
                    )
                    .collect_vec()
                    .into(),
            )
    }
}

impl PartialOrd for CodeAction {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CodeAction {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.title.cmp(&other.title)
    }
}

impl TryFrom<lsp_types::CodeAction> for CodeAction {
    type Error = anyhow::Error;

    fn try_from(value: lsp_types::CodeAction) -> Result<Self, Self::Error> {
        log::info!("CodeAction: {value:#?}");

        let title = value.title;
        Ok(CodeAction {
            title,
            kind: value.kind.map(|kind| kind.as_str().to_string()),
            edit: value.edit.map(WorkspaceEdit::try_from).transpose()?,
            command: value.command.map(Command),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_only_action_is_executable() {
        let action = CodeAction::from(lsp_types::Command {
            title: "Organize imports".to_string(),
            command: "java.edit.organizeImports".to_string(),
            arguments: None,
        });

        assert_eq!(action.title, "Organize imports");
        assert!(action.edit.is_none());
        assert_eq!(
            action.command.unwrap().command(),
            "java.edit.organizeImports"
        );
    }
}
