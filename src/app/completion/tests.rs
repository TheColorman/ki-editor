use super::*;
use crate::{
    frontend::{mock::MockFrontend, NullWriter},
    lsp::{completion::session::CompletionResponse, manager::LspServerKey},
};

fn fixture() -> anyhow::Result<(App<MockFrontend>, tempfile::TempDir)> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("Component.vue");
    std::fs::write(&path, "")?;
    let mut app = App::new(
        Rc::new(Mutex::new(MockFrontend::new(Box::new(NullWriter)))),
        temp.path().try_into()?,
        Vec::new(),
        RunTestOptions {
            enable_lsp: false,
            enable_syntax_highlighting: false,
            enable_file_watcher: false,
        },
    )?;
    app.handle_dispatch(Dispatch::OpenFile {
        path: path.try_into()?,
        owner: BufferOwner::User,
        focus: true,
    })?;
    app.handle_dispatch(Dispatch::ToEditor(EnterInsertMode(Direction::Start)))?;
    Ok((app, temp))
}

fn request(app: &mut App<MockFrontend>) -> anyhow::Result<CompletionRequest> {
    app.handle_dispatch(Dispatch::RequestCompletion)?;
    app.handle_dispatch(Dispatch::RequestCompletionDebounced)?;
    Ok(app.completion_session.request().unwrap().clone())
}

fn reply(request: &CompletionRequest) -> LspNotification {
    LspNotification::Completion(CompletionResponse {
        request: request.clone(),
        server: LspServerKey {
            language_id: "vue".to_string(),
            server_id: "tailwindcss".to_string(),
            root: request.path.parent().unwrap().unwrap(),
        },
        items: vec![lsp_types::CompletionItem {
            label: "flex".to_string(),
            ..Default::default()
        }],
        trigger_characters: Vec::new(),
    })
}

fn current_item(app: &mut App<MockFrontend>) -> Option<DropdownItem> {
    app.current_component()
        .borrow_mut()
        .as_any_mut()
        .downcast_mut::<SuggestiveEditor>()
        .unwrap()
        .completion_dropdown_current_item()
}

#[test]
fn late_completion_cannot_reopen_after_escape() -> anyhow::Result<()> {
    let (mut app, _temp) = fixture()?;
    let old = request(&mut app)?;
    app.handle_dispatch(Dispatch::HandleKeyEvent(key!("esc")))?;
    app.handle_dispatch(Dispatch::ToEditor(EnterInsertMode(Direction::Start)))?;
    app.handle_lsp_notification(reply(&old))?;
    assert!(current_item(&mut app).is_none());
    assert!(app.completion_session.request().is_none());
    Ok(())
}

#[test]
fn completion_rejects_previous_document_version_and_cursor_position() -> anyhow::Result<()> {
    let (mut app, _temp) = fixture()?;
    let old = request(&mut app)?;
    app.handle_dispatch(Dispatch::ToEditor(SetContent("changed".to_string())))?;
    app.handle_lsp_notification(reply(&old))?;
    assert!(current_item(&mut app).is_none());
    let new = request(&mut app)?;
    let mut wrong_position = new.clone();
    wrong_position.position = Position::new(100, 100);
    app.handle_lsp_notification(reply(&wrong_position))?;
    assert!(current_item(&mut app).is_none());
    app.handle_lsp_notification(reply(&new))?;
    assert!(current_item(&mut app).unwrap().display().contains("flex"));
    Ok(())
}

#[test]
fn switching_files_invalidates_pending_and_active_completions() -> anyhow::Result<()> {
    let (mut app, temp) = fixture()?;
    let old = request(&mut app)?;
    app.handle_dispatch(Dispatch::RequestCompletion)?;
    let other = temp.path().join("Other.vue");
    std::fs::write(&other, "")?;
    app.handle_dispatch(Dispatch::OpenFile {
        path: other.try_into()?,
        owner: BufferOwner::User,
        focus: true,
    })?;
    app.handle_dispatch(Dispatch::ToEditor(EnterInsertMode(Direction::Start)))?;
    app.handle_dispatch(Dispatch::RequestCompletionDebounced)?;
    app.handle_lsp_notification(reply(&old))?;
    assert!(app.completion_session.request().is_none());
    assert!(current_item(&mut app).is_none());
    Ok(())
}

#[test]
fn accepting_completion_invalidates_the_session() -> anyhow::Result<()> {
    let (mut app, _temp) = fixture()?;
    let old = request(&mut app)?;
    app.handle_lsp_notification(reply(&old))?;
    app.handle_dispatch(Dispatch::SelectCompletionItem)?;
    app.handle_lsp_notification(reply(&old))?;
    assert!(app.completion_session.request().is_none());
    assert!(current_item(&mut app).is_none());
    assert_eq!(
        app.current_component().borrow().editor().buffer().content(),
        "flex"
    );
    Ok(())
}
