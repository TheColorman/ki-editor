//! Optional smoke test against an installed server and a real Tailwind project.
//! Buffer contents are sent over LSP; the project files are never modified.

use super::super::*;
use crate::{
    lsp::completion::session::{CompletionItemSource, CompletionRequest},
    position::Position,
};

fn wait_for(
    receiver: &crossbeam_channel::Receiver<AppMessage>,
    predicate: impl Fn(&LspNotification) -> bool,
) -> anyhow::Result<LspNotification> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let message = receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
        if let AppMessage::LspNotification(notification) = message {
            if predicate(&notification) {
                return Ok(*notification);
            }
        }
    }
}

#[test]
#[ignore = "requires tailwindcss-language-server and KI_TAILWIND_TEST_ROOT/KI_TAILWIND_TEST_FILE"]
fn tailwind_completes_vue_classes_and_resolves_documentation() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let root: AbsolutePath = std::env::var("KI_TAILWIND_TEST_ROOT")?
        .as_str()
        .try_into()?;
    let path: AbsolutePath = std::env::var("KI_TAILWIND_TEST_FILE")?
        .as_str()
        .try_into()?;
    let language = crate::config::from_extension("vue").unwrap();
    let config = language
        .lsp_server_configs()
        .into_iter()
        .find(|server| server.id() == "tailwindcss")
        .unwrap();
    let (sender, receiver) = crossbeam_channel::unbounded();
    let mut channel = LspServerProcessChannel::new(language, config, sender, root)?.unwrap();
    wait_for(&receiver, |notification| {
        matches!(notification, LspNotification::Initialized { .. })
    })
    .context("waiting for Tailwind initialization")?;
    channel.initialized();

    for (index, prefix) in ["flex", "bg-", "md:hover:bg-"].into_iter().enumerate() {
        let version = index as i32 + 1;
        let before_cursor = format!("<template><div class=\"{prefix}");
        let content = format!("{before_cursor}\"></div></template>");
        if index == 0 {
            channel.document_did_open(OpenDocument {
                path: path.clone(),
                version,
                content,
            })?;
        } else {
            channel.send_from_editor(FromEditor::TextDocumentDidChange {
                file_path: path.clone(),
                version,
                content,
            })?;
        }
        let position = Position::new(0, before_cursor.len());
        let request = CompletionRequest {
            generation: index as u64,
            path: path.clone(),
            version,
            position,
        };
        let params = RequestParams {
            path: path.clone(),
            position,
            selection_end: position,
            context: ResponseContext {
                completion: Some(Arc::new(CompletionContext::Request(request.clone()))),
                ..Default::default()
            },
        };
        channel.send_from_editor(FromEditor::TextDocumentCompletion(params.clone()))?;
        let response = wait_for(&receiver, |notification| {
            matches!(notification, LspNotification::Completion(_))
        })
        .with_context(|| format!("waiting for Tailwind completion of {prefix:?}"))?;
        let LspNotification::Completion(response) = response else {
            unreachable!()
        };
        anyhow::ensure!(
            response.request == request,
            "response belongs to another request"
        );
        anyhow::ensure!(
            !response.items.is_empty(),
            "no Tailwind completions for {prefix:?}"
        );
        let (item_index, item) = response
            .items
            .iter()
            .enumerate()
            .find(|(_, item)| {
                if index == 0 {
                    item.label == "flex"
                } else {
                    item.label.starts_with("bg-")
                }
            })
            .ok_or_else(|| anyhow::anyhow!("expected Tailwind utility missing for {prefix:?}"))?;
        anyhow::ensure!(
            item.text_edit.is_some(),
            "Tailwind should provide a class replacement range"
        );
        let source = CompletionItemSource {
            request,
            server: response.server,
            index: item_index,
        };
        channel.send_from_editor(FromEditor::CompletionItemResolve {
            completion_item: Box::new(item.clone()),
            params: RequestParams {
                context: ResponseContext {
                    completion: Some(Arc::new(CompletionContext::Resolve(source))),
                    ..Default::default()
                },
                ..params
            },
        })?;
        let resolved = wait_for(&receiver, |notification| {
            matches!(notification, LspNotification::CompletionItemResolve(..))
        })?;
        let LspNotification::CompletionItemResolve(_, item) = resolved else {
            unreachable!()
        };
        anyhow::ensure!(
            item.documentation.is_some(),
            "Tailwind completion documentation missing"
        );
    }
    channel.request_shutdown()?;
    channel.wait_for_exit_until(Instant::now() + Duration::from_secs(3));
    Ok(())
}
