use itertools::Itertools;

use crate::{
    app::{Dispatch, Dispatches},
    components::dropdown_sync::DropdownItem,
};

use super::workspace_edit::WorkspaceEdit;

#[derive(Debug, Clone, PartialEq)]
/// Refer https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#codeAction
pub struct CodeAction {
    pub title: String,
    pub kind: Option<String>,
    pub edit: Option<WorkspaceEdit>,
    pub command: Option<Command>,
    pub(crate) unresolved: Option<Box<lsp_types::CodeAction>>,
}

impl Eq for CodeAction {}

impl CodeAction {
    pub fn execution_dispatches(self) -> Dispatches {
        self.edit
            .map(Dispatch::ApplyWorkspaceEdit)
            .into_iter()
            // If a code action provides an edit and a command, the edit must run first.
            .chain(
                self.command
                    .map(|command| Dispatch::LspExecuteCommand { command }),
            )
            .collect_vec()
            .into()
    }

    fn selection_dispatches(self) -> Dispatches {
        match self {
            CodeAction {
                unresolved: Some(code_action),
                ..
            } => Dispatches::one(Dispatch::ResolveCodeAction(code_action)),
            code_action => code_action.execution_dispatches(),
        }
    }
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
            unresolved: None,
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
        let title = value.title.clone();
        let group = value
            .kind
            .clone()
            .and_then(|kind| if kind.is_empty() { None } else { Some(kind) })
            .unwrap_or("Misc.".to_string());
        DropdownItem::new(title)
            .set_group(Some(group))
            .set_dispatches(value.selection_dispatches())
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

        let unresolved =
            (value.edit.is_none() && value.data.is_some()).then(|| Box::new(value.clone()));
        Ok(CodeAction {
            title: value.title,
            kind: value.kind.map(|kind| kind.as_str().to_string()),
            edit: value.edit.map(WorkspaceEdit::try_from).transpose()?,
            command: value.command.map(Command),
            unresolved,
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

    #[test]
    fn unresolved_action_is_resolved_when_selected() -> anyhow::Result<()> {
        let protocol_action = lsp_types::CodeAction {
            title: "using System.Text.Json;".to_string(),
            kind: Some(lsp_types::CodeActionKind::QUICKFIX),
            data: Some(serde_json::json!({ "Identifier": "using System.Text.Json;" })),
            ..Default::default()
        };

        let dropdown_item = DropdownItem::from(CodeAction::try_from(protocol_action.clone())?);

        assert_eq!(
            dropdown_item.dispatches.into_vec(),
            vec![Dispatch::ResolveCodeAction(Box::new(protocol_action))]
        );
        Ok(())
    }
}
