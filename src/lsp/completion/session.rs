//! Completion results belong to a document snapshot, a request, and a server.
//! Keep that ownership until insertion/resolve rather than merging raw LSP items.

use shared::absolute_path::AbsolutePath;

use crate::{components::dropdown_sync::DropdownItem, position::Position};

use super::{Completion, CompletionItem};
use crate::lsp::manager::LspServerKey;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionRequest {
    pub generation: u64,
    pub path: AbsolutePath,
    pub version: i32,
    pub position: Position,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItemSource {
    pub request: CompletionRequest,
    pub server: LspServerKey,
    pub index: usize,
}

impl CompletionItemSource {
    pub fn dropdown_id(&self) -> String {
        // A session is scoped to one document, hence one language/workspace.
        format!(
            "completion:{}:{}:{}",
            self.request.generation, self.server.server_id, self.index
        )
    }
}

/// Carried through the existing pending-request context, never sent to the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionContext {
    Request(CompletionRequest),
    Resolve(CompletionItemSource),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompletionResponse {
    pub request: CompletionRequest,
    pub server: LspServerKey,
    pub items: Vec<lsp_types::CompletionItem>,
    pub trigger_characters: Vec<String>,
}

struct ServerCompletion {
    server: LspServerKey,
    completion: Completion,
}

struct ActiveCompletion {
    request: CompletionRequest,
    results: Vec<ServerCompletion>,
}

#[derive(Default)]
pub struct CompletionSession {
    generation: u64,
    active: Option<ActiveCompletion>,
}

impl CompletionSession {
    pub fn begin(
        &mut self,
        path: AbsolutePath,
        version: i32,
        position: Position,
    ) -> CompletionRequest {
        self.generation += 1;
        let request = CompletionRequest {
            generation: self.generation,
            path,
            version,
            position,
        };
        self.active = Some(ActiveCompletion {
            request: request.clone(),
            results: Vec::new(),
        });
        request
    }

    pub fn request(&self) -> Option<&CompletionRequest> {
        self.active.as_ref().map(|active| &active.request)
    }

    pub fn invalidate(&mut self) {
        self.active = None;
    }

    pub fn receive(&mut self, response: CompletionResponse) -> Option<Completion> {
        let active = self.active.as_mut()?;
        if active.request != response.request {
            return None;
        }
        let completion = Completion {
            items: response
                .items
                .into_iter()
                .enumerate()
                .map(|(index, item)| {
                    CompletionItem::from(item).into_dropdown_from(CompletionItemSource {
                        request: response.request.clone(),
                        server: response.server.clone(),
                        index,
                    })
                })
                .collect(),
            trigger_characters: response.trigger_characters,
        };
        active
            .results
            .retain(|result| result.server != response.server);
        active.results.push(ServerCompletion {
            server: response.server,
            completion,
        });
        // Response timing must not change the order of equally ranked items.
        active
            .results
            .sort_by(|a, b| a.server.server_id.cmp(&b.server.server_id));
        Some(active.combined())
    }

    pub fn resolve(
        &mut self,
        source: CompletionItemSource,
        item: lsp_types::CompletionItem,
    ) -> Option<Completion> {
        let active = self.active.as_mut()?;
        if active.request != source.request {
            return None;
        }
        let result = active
            .results
            .iter_mut()
            .find(|result| result.server == source.server)?;
        let target = result.completion.items.get_mut(source.index)?;
        *target = CompletionItem::from(item)
            .into_dropdown_from(source)
            .set_resolved(true);
        Some(active.combined())
    }
}

impl ActiveCompletion {
    fn combined(&self) -> Completion {
        Completion {
            items: self
                .results
                .iter()
                .flat_map(|result| result.completion.items.clone())
                .collect::<Vec<DropdownItem>>(),
            trigger_characters: self
                .results
                .iter()
                .flat_map(|result| result.completion.trigger_characters.clone())
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Dispatch;

    fn begin(session: &mut CompletionSession) -> CompletionRequest {
        session.begin(
            std::env::current_dir()
                .unwrap()
                .join("Component.vue")
                .try_into()
                .unwrap(),
            1,
            Position::new(0, 12),
        )
    }

    fn response(request: &CompletionRequest, server: &str, labels: &[&str]) -> CompletionResponse {
        CompletionResponse {
            request: request.clone(),
            server: LspServerKey {
                language_id: "vue".to_string(),
                server_id: server.to_string(),
                root: std::env::current_dir().unwrap().try_into().unwrap(),
            },
            items: labels
                .iter()
                .map(|label| lsp_types::CompletionItem {
                    label: label.to_string(),
                    data: Some(serde_json::json!({"_projectKey": server})),
                    ..Default::default()
                })
                .collect(),
            trigger_characters: vec![":".to_string()],
        }
    }

    fn labels(completion: &Completion) -> Vec<String> {
        completion.items.iter().map(DropdownItem::display).collect()
    }

    #[test]
    fn completion_responses_merge_in_either_order_and_empty_is_per_server() {
        for order in [["vue", "tailwindcss"], ["tailwindcss", "vue"]] {
            let mut session = CompletionSession::default();
            let request = begin(&mut session);
            session
                .receive(response(&request, order[0], &[order[0]]))
                .unwrap();
            let merged = session
                .receive(response(&request, order[1], &[order[1]]))
                .unwrap();
            assert_eq!(merged.items.len(), 2);
            assert!(labels(&merged)[0].contains("tailwindcss"));
            assert!(labels(&merged)[1].contains("vue"));
            let remaining = session
                .receive(response(&request, "tailwindcss", &[]))
                .unwrap();
            assert_eq!(remaining.items.len(), 1);
            assert!(labels(&remaining)[0].contains("vue"));
        }
    }

    #[test]
    fn completion_sessions_reject_old_responses_and_resolves_after_dismissal() {
        let mut session = CompletionSession::default();
        let old = begin(&mut session);
        let reply = response(&old, "tailwindcss", &["flex"]);
        let source = CompletionItemSource {
            request: old.clone(),
            server: reply.server.clone(),
            index: 0,
        };
        let new = begin(&mut session);
        assert_ne!(old.generation, new.generation);
        assert!(session.receive(reply).is_none());
        assert!(session
            .resolve(source, lsp_types::CompletionItem::default())
            .is_none());
        session.invalidate();
        assert!(session
            .receive(response(&new, "tailwindcss", &["flex"]))
            .is_none());
    }

    #[test]
    fn same_label_items_keep_owner_data_and_resolve_independently() {
        let mut session = CompletionSession::default();
        let request = begin(&mut session);
        session.receive(response(&request, "vue", &["flex"]));
        let tailwind = response(&request, "tailwindcss", &["flex"]);
        let source = CompletionItemSource {
            request: request.clone(),
            server: tailwind.server.clone(),
            index: 0,
        };
        let merged = session.receive(tailwind.clone()).unwrap();
        for item in &merged.items {
            let dispatches = item.on_focused().into_vec();
            let Dispatch::ResolveCompletionItem {
                completion_item,
                source: Some(source),
            } = &dispatches[0]
            else {
                panic!("completion must retain its originating server");
            };
            assert_eq!(
                completion_item.data.as_ref().unwrap()["_projectKey"],
                source.server.server_id
            );
        }
        let resolved = lsp_types::CompletionItem {
            detail: Some("display: flex".to_string()),
            ..tailwind.items[0].clone()
        };
        let merged = session.resolve(source, resolved).unwrap();
        assert!(merged.items[0].resolved());
        assert!(!merged.items[1].resolved());
        // A late result from another server must not undo resolution.
        let merged = session
            .receive(response(&request, "vue", &["flex", "ref"]))
            .unwrap();
        assert!(merged.items[0].resolved());
    }

    #[test]
    fn completion_commands_are_addressed_to_the_originating_server() {
        let mut session = CompletionSession::default();
        let request = begin(&mut session);
        let mut reply = response(&request, "tailwindcss", &["flex"]);
        reply.items[0].command = Some(lsp_types::Command::new(
            "test".to_string(),
            "test.command".to_string(),
            None,
        ));
        let server = reply.server.clone();
        let merged = session.receive(reply).unwrap();
        assert!(merged.items[0].dispatches.clone().into_vec().iter().any(|dispatch| matches!(
            dispatch, Dispatch::LspExecuteCommand { server: Some(owner), .. } if owner == &server
        )));
    }
}
