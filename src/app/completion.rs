//! Connect the focused editor to completion sessions without letting asynchronous
//! replies outlive the document/cursor state that requested them.

use super::*;
use crate::lsp::completion::session::CompletionItemSource;

#[cfg(test)]
mod tests;

impl<T: Frontend> App<T> {
    fn completion_snapshot(&self) -> Option<(RequestParams, i32)> {
        let params = self.get_request_params()?;
        let component = self.current_component();
        let component = component.borrow();
        let editor = component.editor();
        if editor.mode != crate::components::editor::Mode::Insert {
            return None;
        }
        let version = editor.buffer().lsp_document_version();
        Some((params, version))
    }

    fn completion_request_is_current(&self, request: &CompletionRequest) -> bool {
        self.completion_snapshot().is_some_and(|(params, version)| {
            params.path == request.path
                && params.position == request.position
                && version == request.version
        })
    }

    pub(super) fn invalidate_stale_completion(&mut self) {
        if self
            .completion_session
            .request()
            .is_some_and(|request| !self.completion_request_is_current(request))
        {
            self.completion_session.invalidate();
        }
        if self
            .pending_completion
            .as_ref()
            .is_some_and(|(pending, version)| {
                !self
                    .completion_snapshot()
                    .is_some_and(|(current, current_version)| {
                        current.path == pending.path
                            && current.position == pending.position
                            && current_version == *version
                    })
            })
        {
            self.pending_completion = None;
        }
    }

    pub(super) fn request_completion(&mut self) {
        self.completion_session.invalidate();
        self.pending_completion = self.completion_snapshot();
        self.debounce_lsp_request_completion.call(());
    }

    pub(super) fn dismiss_completion(&mut self) {
        self.completion_session.invalidate();
        self.pending_completion = None;
    }

    pub(super) fn request_completion_debounced(&mut self) -> anyhow::Result<()> {
        let Some((mut params, version)) = self.pending_completion.take() else {
            return Ok(());
        };
        let request = self
            .completion_session
            .begin(params.path.clone(), version, params.position);
        params.context.completion = Some(Arc::new(CompletionContext::Request(request)));
        self.lsp_manager().send_message(
            params.path.clone(),
            FromEditor::TextDocumentCompletion(params),
        )
    }

    pub(super) fn resolve_completion_item(
        &mut self,
        completion_item: lsp_types::CompletionItem,
        source: Option<CompletionItemSource>,
    ) -> anyhow::Result<()> {
        let Some(mut params) = self.get_request_params() else {
            return Ok(());
        };
        match source {
            Some(source) => {
                if self.completion_session.request() != Some(&source.request) {
                    return Ok(());
                }
                params.context.completion =
                    Some(Arc::new(CompletionContext::Resolve(source.clone())));
                self.lsp_manager().send_to_server(
                    &source.server,
                    FromEditor::CompletionItemResolve {
                        completion_item: Box::new(completion_item),
                        params,
                    },
                )
            }
            None => self.lsp_manager().send_message(
                params.path.clone(),
                FromEditor::CompletionItemResolve {
                    completion_item: Box::new(completion_item),
                    params,
                },
            ),
        }
    }
}
