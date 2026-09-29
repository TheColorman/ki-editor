use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use crate::app::AppMessage;
use shared::language_servers::configuration::ServerConfiguration;

use super::process::{FromEditor, LspServerProcessChannel, OpenDocument};
use shared::{
    absolute_path::AbsolutePath,
    language::{Language, LspServerConfig},
};

const LSP_RESTART_BASE_DELAY: Duration = Duration::from_millis(250);
const LSP_RESTART_MAX_DELAY: Duration = Duration::from_secs(30);
const LSP_STABLE_UPTIME: Duration = Duration::from_secs(30);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attached_servers_choose_their_own_roots() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        std::fs::create_dir_all(project.join("src"))?;
        std::fs::write(project.join("pom.xml"), "")?;
        let path: AbsolutePath = project.join("src/Component.vue").try_into()?;
        let mut language = serde_json::to_value(crate::config::from_extension("vue").unwrap())?;
        language["lsp_servers"] = serde_json::json!([
            { "id": "jdtls", "command": { "command": "custom-wrapper", "arguments": [] }, "primary": true },
            { "id": "other", "command": { "command": "other-server", "arguments": [] }, "completion": true }
        ]);
        let language: Language = serde_json::from_value(language)?;
        let (sender, _receiver) = crossbeam_channel::unbounded();
        let manager = LspManager::new(sender, temp.path().try_into()?);
        assert_eq!(
            manager
                .lsp_root_for_server(&language, "jdtls", &path)
                .unwrap()
                .as_ref(),
            project
        );
        assert_eq!(
            manager
                .lsp_root_for_server(&language, "other", &path)
                .unwrap()
                .as_ref(),
            temp.path()
        );
        assert!(manager
            .lsp_root_for_server(&language, "unknown", &path)
            .is_none());
        Ok(())
    }

    #[test]
    fn secondary_completion_routing_preserves_primary_navigation() -> anyhow::Result<()> {
        let path: AbsolutePath = std::env::current_dir()?.join("Component.vue").try_into()?;
        let params = crate::app::RequestParams {
            path: path.clone(),
            position: crate::position::Position::default(),
            selection_end: crate::position::Position::default(),
            context: crate::lsp::process::ResponseContext::default(),
        };
        let servers = crate::config::from_extension("vue")
            .unwrap()
            .lsp_server_configs();
        let recipients = |message| {
            servers
                .iter()
                .filter(|server| LspManager::server_receives_message(server, &message))
                .map(|server| server.id().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            recipients(FromEditor::TextDocumentCompletion(params.clone())),
            ["vue", "tailwindcss"]
        );
        assert_eq!(recipients(FromEditor::TextDocumentHover(params)), ["vue"]);
        assert_eq!(
            recipients(FromEditor::TextDocumentDidClose { file_path: path }),
            ["vue", "eslint", "tailwindcss"]
        );
        Ok(())
    }

    fn manager_with_disconnected_server(path: &AbsolutePath) -> anyhow::Result<LspManager> {
        let language = crate::config::from_path(path)
            .ok_or_else(|| anyhow::anyhow!("test path has no configured language"))?;
        let config = language
            .lsp_server_configs()
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("test language has no configured LSP server"))?;
        let root: AbsolutePath = path
            .as_ref()
            .parent()
            .ok_or_else(|| anyhow::anyhow!("test path has no parent"))?
            .try_into()?;
        let (sender, _receiver) = crossbeam_channel::unbounded();
        let mut manager = LspManager::new(sender, root.clone());
        let key = LspManager::server_key(&language, config.id(), root.clone())
            .ok_or_else(|| anyhow::anyhow!("test language has no ID"))?;
        manager.lsp_server_process_channels.insert(
            key,
            LspServerProcessChannel::disconnected_for_test(language, config),
        );
        Ok(manager)
    }

    #[test]
    fn did_close_is_a_lifecycle_message() -> anyhow::Result<()> {
        let path = std::env::current_dir()?.try_into()?;
        assert!(LspManager::is_lifecycle_message(
            &FromEditor::TextDocumentDidClose { file_path: path }
        ));
        Ok(())
    }

    #[test]
    fn disconnected_server_does_not_fail_document_lifecycle() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let path: AbsolutePath = tempdir.path().join("main.rs").try_into()?;
        let mut manager = manager_with_disconnected_server(&path)?;

        manager.send_message(
            path.clone(),
            FromEditor::TextDocumentDidChange {
                file_path: path.clone(),
                version: 1,
                content: "fn main() {}".to_string(),
            },
        )?;

        assert!(manager.lsp_server_process_channels.is_empty());
        assert_eq!(manager.restart_states.len(), 1);
        assert!(!manager.should_restart_for_path(&path));
        manager
            .restart_states
            .values_mut()
            .for_each(|state| state.retry_at = Instant::now());
        assert!(manager.should_restart_for_path(&path));
        Ok(())
    }

    #[test]
    fn disconnected_server_still_fails_interactive_request() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let path: AbsolutePath = tempdir.path().join("main.rs").try_into()?;
        let mut manager = manager_with_disconnected_server(&path)?;
        let result = manager.send_message(
            path.clone(),
            FromEditor::TextDocumentHover(crate::app::RequestParams {
                path,
                position: crate::position::Position::default(),
                selection_end: crate::position::Position::default(),
                context: crate::lsp::process::ResponseContext::default(),
            }),
        );

        let error = result.unwrap_err().to_string();
        assert!(error.contains("stopped"));
        assert!(!error.contains("closed channel"));
        assert!(manager.lsp_server_process_channels.is_empty());
        assert_eq!(manager.restart_states.len(), 1);
        Ok(())
    }

    #[test]
    fn restart_delay_is_exponential_and_bounded() {
        assert_eq!(LspManager::restart_delay(1), Duration::from_millis(250));
        assert_eq!(LspManager::restart_delay(2), Duration::from_millis(500));
        assert_eq!(LspManager::restart_delay(8), Duration::from_secs(30));
        assert_eq!(LspManager::restart_delay(u32::MAX), Duration::from_secs(30));
    }

    #[test]
    fn lsp_root_prefers_nested_package_workspace_root() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let root: AbsolutePath = tempdir.path().try_into()?;
        let frontend = tempdir.path().join("frontends");
        let app = frontend.join("apps/launch/app");
        std::fs::create_dir_all(&app)?;
        std::fs::write(frontend.join("pnpm-workspace.yaml"), "packages: []")?;
        std::fs::write(app.join("app.vue"), "<template />")?;

        let (sender, _receiver) = crossbeam_channel::unbounded();
        let manager = LspManager::new(sender, root);
        let language = crate::config::from_extension("vue").unwrap();
        let actual = manager.lsp_root_for_path(&language, &app.join("app.vue").try_into()?);

        assert_eq!(actual.as_ref(), frontend.as_path());
        Ok(())
    }

    #[test]
    fn lsp_root_treats_pnpm_lockfile_as_package_root() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let root: AbsolutePath = tempdir.path().try_into()?;
        let frontend = tempdir.path().join("frontends");
        let app = frontend.join("apps/launch/app");
        std::fs::create_dir_all(&app)?;
        std::fs::write(frontend.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'")?;
        std::fs::write(app.join("app.vue"), "<template />")?;

        let (sender, _receiver) = crossbeam_channel::unbounded();
        let manager = LspManager::new(sender, root);
        let language = crate::config::from_extension("vue").unwrap();
        let actual = manager.lsp_root_for_path(&language, &app.join("app.vue").try_into()?);

        assert_eq!(actual.as_ref(), frontend.as_path());
        Ok(())
    }

    #[test]
    fn package_marker_priority_depends_on_the_server_policy() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let workspace = tempdir.path().join("workspace");
        let package = workspace.join("package");
        let source = package.join("src");
        std::fs::create_dir_all(&source)?;
        std::fs::write(workspace.join("pnpm-workspace.yaml"), "")?;
        std::fs::write(package.join("package.json"), "{}")?;
        let (sender, _receiver) = crossbeam_channel::unbounded();
        let manager = LspManager::new(sender, tempdir.path().try_into()?);

        let vue = crate::config::from_extension("vue").unwrap();
        let csharp = crate::config::from_extension("cs").unwrap();
        assert_eq!(
            manager
                .lsp_root_for_path(&vue, &source.join("App.vue").try_into()?)
                .as_ref(),
            workspace
        );
        // OmniSharp puts both package markers in the same fallback group.
        assert_eq!(
            manager
                .lsp_root_for_path(&csharp, &source.join("App.cs").try_into()?)
                .as_ref(),
            package
        );

        // A C# project outranks a nearer package fallback; a solution outranks both.
        std::fs::write(workspace.join("App.CSPROJ"), "")?;
        assert_eq!(
            manager
                .lsp_root_for_path(&csharp, &source.join("App.cs").try_into()?)
                .as_ref(),
            workspace
        );
        std::fs::write(tempdir.path().join("App.SLNX"), "")?;
        assert_eq!(
            manager
                .lsp_root_for_path(&csharp, &source.join("App.cs").try_into()?)
                .as_ref(),
            tempdir.path()
        );
        Ok(())
    }

    #[test]
    fn csharp_lsp_root_prefers_solution_over_project() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let root: AbsolutePath = tempdir.path().try_into()?;
        let solution = tempdir.path().join("services/api");
        let project = solution.join("src/App");
        std::fs::create_dir_all(&project)?;
        std::fs::write(solution.join("App.sln"), "")?;
        std::fs::write(project.join("App.csproj"), "")?;
        std::fs::write(project.join("Program.cs"), "")?;

        let (sender, _receiver) = crossbeam_channel::unbounded();
        let manager = LspManager::new(sender, root);
        let language = crate::config::from_extension("cs").unwrap();
        let actual = manager.lsp_root_for_path(&language, &project.join("Program.cs").try_into()?);

        assert_eq!(actual.as_ref(), solution);
        Ok(())
    }

    #[test]
    fn csharp_lsp_root_falls_back_to_nearest_project() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let root: AbsolutePath = tempdir.path().try_into()?;
        let project = tempdir.path().join("src/App");
        std::fs::create_dir_all(&project)?;
        std::fs::write(project.join("App.csproj"), "")?;
        std::fs::write(project.join("Program.cs"), "")?;

        let (sender, _receiver) = crossbeam_channel::unbounded();
        let manager = LspManager::new(sender, root);
        let language = crate::config::from_extension("cs").unwrap();
        let actual = manager.lsp_root_for_path(&language, &project.join("Program.cs").try_into()?);

        assert_eq!(actual.as_ref(), project);
        Ok(())
    }

    #[test]
    fn java_lsp_root_falls_back_to_maven_project() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let root: AbsolutePath = tempdir.path().try_into()?;
        let project = tempdir.path().join("service");
        let source = project.join("src/main/java/App.java");
        std::fs::create_dir_all(source.parent().unwrap())?;
        std::fs::write(project.join("pom.xml"), "")?;
        std::fs::write(&source, "class App {}")?;

        let (sender, _receiver) = crossbeam_channel::unbounded();
        let manager = LspManager::new(sender, root);
        let language = crate::config::from_extension("java").unwrap();

        assert_eq!(
            manager
                .lsp_root_for_path(&language, &source.try_into()?)
                .as_ref(),
            project
        );
        Ok(())
    }

    #[test]
    fn java_lsp_root_falls_back_to_gradle_projects() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let root: AbsolutePath = tempdir.path().try_into()?;
        let (sender, _receiver) = crossbeam_channel::unbounded();
        let manager = LspManager::new(sender, root);
        let language = crate::config::from_extension("java").unwrap();

        for (name, marker) in [("groovy", "build.gradle"), ("kotlin", "build.gradle.kts")] {
            let project = tempdir.path().join(name);
            let source = project.join("src/main/java/App.java");
            std::fs::create_dir_all(source.parent().unwrap())?;
            std::fs::write(project.join(marker), "")?;
            std::fs::write(&source, "class App {}")?;

            assert_eq!(
                manager
                    .lsp_root_for_path(&language, &source.try_into()?)
                    .as_ref(),
                project
            );
        }
        Ok(())
    }

    #[test]
    fn java_lsp_root_prefers_wrapper_over_child_project() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let root: AbsolutePath = tempdir.path().try_into()?;
        let project = tempdir.path().join("services/app");
        let source = project.join("src/App.java");
        std::fs::create_dir_all(source.parent().unwrap())?;
        std::fs::write(tempdir.path().join("gradlew"), "")?;
        std::fs::write(project.join("pom.xml"), "")?;
        std::fs::write(&source, "class App {}")?;

        let (sender, _receiver) = crossbeam_channel::unbounded();
        let manager = LspManager::new(sender, root);
        let language = crate::config::from_extension("java").unwrap();

        assert_eq!(
            manager
                .lsp_root_for_path(&language, &source.try_into()?)
                .as_ref(),
            tempdir.path()
        );
        Ok(())
    }

    #[test]
    fn java_lsp_root_prefers_nearest_nested_settings() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let root: AbsolutePath = tempdir.path().try_into()?;
        let nested = tempdir.path().join("platform");
        let source = nested.join("app/src/App.java");
        std::fs::create_dir_all(source.parent().unwrap())?;
        std::fs::write(tempdir.path().join("settings.gradle"), "")?;
        std::fs::write(nested.join("settings.gradle.kts"), "")?;
        std::fs::write(&source, "class App {}")?;

        let (sender, _receiver) = crossbeam_channel::unbounded();
        let manager = LspManager::new(sender, root);
        let language = crate::config::from_extension("java").unwrap();

        assert_eq!(
            manager
                .lsp_root_for_path(&language, &source.try_into()?)
                .as_ref(),
            nested
        );
        Ok(())
    }

    #[test]
    fn java_lsp_root_recognizes_all_workspace_markers() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let root: AbsolutePath = tempdir.path().try_into()?;
        let (sender, _receiver) = crossbeam_channel::unbounded();
        let manager = LspManager::new(sender, root);
        let language = crate::config::from_extension("java").unwrap();

        for (index, marker) in [
            "settings.gradle",
            "settings.gradle.kts",
            "gradlew",
            "gradlew.bat",
            "mvnw",
            "mvnw.cmd",
            ".mvn",
        ]
        .into_iter()
        .enumerate()
        {
            let project = tempdir.path().join(format!("project-{index}"));
            let source = project.join("src/App.java");
            std::fs::create_dir_all(source.parent().unwrap())?;
            if marker == ".mvn" {
                std::fs::create_dir(project.join(marker))?;
            } else {
                std::fs::write(project.join(marker), "")?;
            }
            std::fs::write(&source, "class App {}")?;

            assert_eq!(
                manager
                    .lsp_root_for_path(&language, &source.try_into()?)
                    .as_ref(),
                project
            );
        }
        Ok(())
    }

    #[test]
    fn java_lsp_root_ignores_node_package_markers() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let root: AbsolutePath = tempdir.path().try_into()?;
        let source = tempdir.path().join("web/src/App.java");
        std::fs::create_dir_all(source.parent().unwrap())?;
        std::fs::write(tempdir.path().join("pnpm-workspace.yaml"), "packages: []")?;
        std::fs::write(source.parent().unwrap().join("package.json"), "{}")?;
        std::fs::write(&source, "class App {}")?;

        let (sender, _receiver) = crossbeam_channel::unbounded();
        let manager = LspManager::new(sender, root);
        let language = crate::config::from_extension("java").unwrap();

        assert_eq!(
            manager
                .lsp_root_for_path(&language, &source.as_path().try_into()?)
                .as_ref(),
            source.parent().unwrap()
        );
        Ok(())
    }

    #[test]
    fn java_lsp_root_for_standalone_file_is_file_parent() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let root: AbsolutePath = tempdir.path().try_into()?;
        let source = tempdir.path().join("scratch/deep/App.java");
        std::fs::create_dir_all(source.parent().unwrap())?;
        std::fs::write(&source, "class App {}")?;

        let (sender, _receiver) = crossbeam_channel::unbounded();
        let manager = LspManager::new(sender, root);
        let language = crate::config::from_extension("java").unwrap();

        assert_eq!(
            manager
                .lsp_root_for_path(&language, &source.as_path().try_into()?)
                .as_ref(),
            source.parent().unwrap()
        );
        Ok(())
    }

    #[test]
    fn java_lsp_root_does_not_search_above_cwd() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let cwd = tempdir.path().join("workspace");
        let source = cwd.join("scratch/App.java");
        std::fs::create_dir_all(source.parent().unwrap())?;
        std::fs::write(tempdir.path().join("settings.gradle"), "")?;
        std::fs::write(&source, "class App {}")?;

        let (sender, _receiver) = crossbeam_channel::unbounded();
        let manager = LspManager::new(sender, cwd.try_into()?);
        let language = crate::config::from_extension("java").unwrap();

        assert_eq!(
            manager
                .lsp_root_for_path(&language, &source.as_path().try_into()?)
                .as_ref(),
            source.parent().unwrap()
        );
        Ok(())
    }

    #[test]
    fn java_lsp_root_outside_cwd_searches_file_ancestors() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let cwd = tempdir.path().join("cwd");
        let project = tempdir.path().join("outside/service");
        let source = project.join("src/App.java");
        std::fs::create_dir(&cwd)?;
        std::fs::create_dir_all(source.parent().unwrap())?;
        std::fs::write(project.join("pom.xml"), "")?;
        std::fs::write(&source, "class App {}")?;

        let (sender, _receiver) = crossbeam_channel::unbounded();
        let manager = LspManager::new(sender, cwd.try_into()?);
        let language = crate::config::from_extension("java").unwrap();

        assert_eq!(
            manager
                .lsp_root_for_path(&language, &source.try_into()?)
                .as_ref(),
            project
        );
        Ok(())
    }

    #[test]
    fn java_lsp_root_outside_cwd_standalone_falls_back_to_file_parent() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let cwd = tempdir.path().join("cwd");
        let source = tempdir.path().join("outside/scratch/App.java");
        std::fs::create_dir(&cwd)?;
        std::fs::create_dir_all(source.parent().unwrap())?;
        std::fs::write(&source, "class App {}")?;

        let (sender, _receiver) = crossbeam_channel::unbounded();
        let manager = LspManager::new(sender, cwd.try_into()?);
        let language = crate::config::from_extension("java").unwrap();

        assert_eq!(
            manager
                .lsp_root_for_path(&language, &source.as_path().try_into()?)
                .as_ref(),
            source.parent().unwrap()
        );
        Ok(())
    }

    #[test]
    fn java_lsp_root_keeps_projects_distinct() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let root: AbsolutePath = tempdir.path().try_into()?;
        let (sender, _receiver) = crossbeam_channel::unbounded();
        let manager = LspManager::new(sender, root);
        let language = crate::config::from_extension("java").unwrap();
        let mut roots = Vec::new();

        for name in ["api", "worker"] {
            let project = tempdir.path().join(name);
            let source = project.join("src/App.java");
            std::fs::create_dir_all(source.parent().unwrap())?;
            std::fs::write(project.join("pom.xml"), "")?;
            std::fs::write(&source, "class App {}")?;
            roots.push(manager.lsp_root_for_path(&language, &source.try_into()?));
        }

        assert_ne!(roots[0], roots[1]);
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LspServerKey {
    pub language_id: String,
    pub server_id: String,
    pub root: AbsolutePath,
}

struct RestartState {
    failures: u32,
    retry_at: Instant,
    started_at: Option<Instant>,
}

pub struct LspManager {
    lsp_server_process_channels: HashMap<LspServerKey, LspServerProcessChannel>,
    restart_states: HashMap<LspServerKey, RestartState>,
    sender: crossbeam_channel::Sender<AppMessage>,
    current_working_directory: AbsolutePath,
    #[cfg(test)]
    /// Used for testing the correctness of LSP requests
    /// We use HashMap instead of Vec because we only one to store the latest
    /// requests of the same kind
    history: HashMap</* request name */ &'static str, FromEditor>,

    #[cfg(test)]
    /// Used for testing the correctness of initialization
    lsp_server_initialized_args_history: Vec<(String, Vec<AbsolutePath>)>,
}

impl Drop for LspManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl LspManager {
    pub fn new(
        sender: crossbeam_channel::Sender<AppMessage>,
        current_working_directory: AbsolutePath,
    ) -> LspManager {
        LspManager {
            lsp_server_process_channels: HashMap::new(),
            restart_states: HashMap::new(),
            sender,
            current_working_directory,
            #[cfg(test)]
            history: HashMap::default(),
            #[cfg(test)]
            lsp_server_initialized_args_history: Vec::default(),
        }
    }

    fn server_key(
        language: &Language,
        server_id: &str,
        root: AbsolutePath,
    ) -> Option<LspServerKey> {
        Some(LspServerKey {
            language_id: language.id()?.to_string(),
            server_id: server_id.to_string(),
            root,
        })
    }

    fn restart_delay(failures: u32) -> Duration {
        let multiplier = 1_u32
            .checked_shl(failures.saturating_sub(1).min(16))
            .unwrap_or(u32::MAX);
        LSP_RESTART_BASE_DELAY
            .saturating_mul(multiplier)
            .min(LSP_RESTART_MAX_DELAY)
    }

    fn record_server_failure(&mut self, key: LspServerKey) {
        let now = Instant::now();
        let state = self.restart_states.entry(key).or_insert(RestartState {
            failures: 0,
            retry_at: now,
            started_at: None,
        });
        if state
            .started_at
            .is_some_and(|started_at| now.duration_since(started_at) >= LSP_STABLE_UPTIME)
        {
            state.failures = 0;
        }
        state.failures = state.failures.saturating_add(1);
        state.retry_at = now + Self::restart_delay(state.failures);
        state.started_at = None;
    }

    fn record_server_started(&mut self, key: LspServerKey) {
        let now = Instant::now();
        self.restart_states
            .entry(key)
            .and_modify(|state| state.started_at = Some(now))
            .or_insert(RestartState {
                failures: 0,
                retry_at: now,
                started_at: Some(now),
            });
    }

    fn server_can_start(&self, key: &LspServerKey) -> bool {
        self.restart_states
            .get(key)
            .is_none_or(|state| state.started_at.is_some() || Instant::now() >= state.retry_at)
    }

    fn should_restart_for_path(&self, path: &AbsolutePath) -> bool {
        let Some(language) = crate::config::from_path(path) else {
            return false;
        };
        language.lsp_server_configs().into_iter().any(|config| {
            let root = self.root_for_config(&config, path);
            Self::server_key(&language, config.id(), root).is_some_and(|key| {
                !self.lsp_server_process_channels.contains_key(&key)
                    && self.restart_states.contains_key(&key)
                    && self.server_can_start(&key)
            })
        })
    }

    fn start_server(
        &mut self,
        language: &Language,
        config: LspServerConfig,
        root: AbsolutePath,
        path: &AbsolutePath,
    ) -> anyhow::Result<()> {
        let Some(server_key) = Self::server_key(language, config.id(), root.clone()) else {
            return Ok(());
        };
        if !self.server_can_start(&server_key) {
            return Ok(());
        }

        let is_primary = config.primary();
        let server_id = config.id().to_string();
        let command = config.process_command().to_string();
        for warning in ServerConfiguration::new(&config, &root).validation_warnings() {
            log::warn!("LSP server '{server_id}': {warning}");
        }
        match LspServerProcessChannel::new(
            language.clone(),
            config,
            self.sender.clone(),
            root.clone(),
        ) {
            Ok(Some(channel)) => {
                log::info!(
                    "Started LSP server '{server_id}' ({command}) for {path:?} with root {root:?}"
                );
                self.lsp_server_process_channels
                    .insert(server_key.clone(), channel);
                self.record_server_started(server_key);
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(error) => {
                self.record_server_failure(server_key);
                if is_primary {
                    log::error!(
                        "Failed to start primary LSP server '{server_id}' ({command}) for {path:?}: {error:?}"
                    );
                    Err(error)
                } else {
                    log::warn!(
                        "Failed to start secondary LSP server '{server_id}' ({command}) for {path:?}: {error:?}"
                    );
                    Ok(())
                }
            }
        }
    }

    fn root_for_config(&self, config: &LspServerConfig, path: &AbsolutePath) -> AbsolutePath {
        let file_parent = path.as_ref().parent().unwrap_or(path.as_ref());
        let boundary = self.current_working_directory.as_ref();
        config
            .definition()
            .root_policy
            .resolve(file_parent, boundary)
            .try_into()
            .unwrap_or_else(|_| self.current_working_directory.clone())
    }

    /// Initialization notifications identify a particular server, whose root may
    /// differ from the roots used by other servers attached to the same document.
    pub(crate) fn lsp_root_for_server(
        &self,
        language: &Language,
        server_id: &str,
        path: &AbsolutePath,
    ) -> Option<AbsolutePath> {
        language
            .lsp_server_configs()
            .iter()
            .find(|config| config.id() == server_id)
            .map(|config| self.root_for_config(config, path))
    }

    #[cfg(test)]
    fn lsp_root_for_path(&self, language: &Language, path: &AbsolutePath) -> AbsolutePath {
        self.root_for_config(&language.lsp_server_configs()[0], path)
    }

    fn is_lifecycle_message(from_editor: &FromEditor) -> bool {
        matches!(
            from_editor,
            FromEditor::TextDocumentDidOpen { .. }
                | FromEditor::TextDocumentDidChange { .. }
                | FromEditor::TextDocumentDidClose { .. }
                | FromEditor::TextDocumentDidSave { .. }
                | FromEditor::WorkspaceDidRenameFiles { .. }
                | FromEditor::WorkspaceDidCreateFiles { .. }
        )
    }

    fn invoke_channels(
        &mut self,
        path: &AbsolutePath,
        from_editor: &FromEditor,
        f: impl Fn(&LspServerProcessChannel) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let Some(language) = crate::config::from_path(path) else {
            return Ok(());
        };
        let configs = language.lsp_server_configs();
        let configs = configs
            .into_iter()
            .filter(|config| Self::server_receives_message(config, from_editor));

        let mut errors = Vec::new();
        for config in configs {
            let root = self.root_for_config(&config, path);
            let Some(key) = Self::server_key(&language, config.id(), root) else {
                continue;
            };
            let result = match self.lsp_server_process_channels.get_mut(&key) {
                Some(channel) => {
                    if channel.is_running() {
                        f(channel)
                    } else {
                        Err(anyhow::anyhow!("LSP server '{}' stopped", config.id()))
                    }
                }
                None => continue,
            };
            if let Err(error) = result {
                self.lsp_server_process_channels.remove(&key);
                self.record_server_failure(key);
                errors.push(error);
            }
        }

        if Self::is_lifecycle_message(from_editor) {
            for error in errors {
                log::warn!(
                    "Failed to notify an LSP server of {}: {error:?}",
                    from_editor.name()
                );
            }
            Ok(())
        } else if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "Failed to send {} to LSP servers: {errors:?}",
                from_editor.name()
            ))
        }
    }

    fn server_receives_message(config: &LspServerConfig, message: &FromEditor) -> bool {
        Self::is_lifecycle_message(message)
            || match message {
                FromEditor::TextDocumentCompletion(_) => config.completion(),
                _ => config.primary(),
            }
    }

    pub fn send_message(
        &mut self,
        path: AbsolutePath,
        from_editor: FromEditor,
    ) -> anyhow::Result<()> {
        #[cfg(test)]
        self.history
            .insert(from_editor.variant(), from_editor.clone());

        if self.should_restart_for_path(&path) {
            let restart_result = self.open_file_inner(
                OpenDocument {
                    path: path.clone(),
                    version: 0,
                    content: String::new(),
                },
                false,
            );
            if let Err(error) = restart_result {
                if Self::is_lifecycle_message(&from_editor) {
                    log::warn!("Failed to restart an LSP server for {path:?}: {error:?}");
                } else {
                    return Err(error);
                }
            }
        }

        self.invoke_channels(&path, &from_editor, |channel| {
            channel.send_from_editor(from_editor.clone())
        })
    }

    /// Resolve and execute completion items only on the server that produced them.
    /// Do not restart a missing server here: its old completion data is no longer valid.
    pub fn send_to_server(
        &mut self,
        server: &LspServerKey,
        message: FromEditor,
    ) -> anyhow::Result<()> {
        if let Some(channel) = self.lsp_server_process_channels.get_mut(server) {
            if channel.is_running() {
                channel.send_from_editor(message)?;
            }
        }
        Ok(())
    }

    /// Open file can do one of the following:
    /// 1. Start a new LSP server process if it is not started yet.
    /// 2. Notify the LSP server process that a new file is opened.
    /// 3. Do nothing if the LSP server process is spawned but not yet initialized.
    pub fn open_file(&mut self, document: OpenDocument) -> Result<(), anyhow::Error> {
        self.open_file_inner(document, true)
    }

    pub fn ensure_file_server(&mut self, document: OpenDocument) -> Result<(), anyhow::Error> {
        self.open_file_inner(document, false)
    }

    fn open_file_inner(
        &mut self,
        document: OpenDocument,
        notify_healthy_server: bool,
    ) -> Result<(), anyhow::Error> {
        let path = &document.path;
        let Some(language) = crate::config::from_path(path) else {
            return Ok(());
        };

        for config in language.lsp_server_configs() {
            let lsp_root = self.root_for_config(&config, path);
            let Some(server_key) = Self::server_key(&language, config.id(), lsp_root.clone())
            else {
                continue;
            };
            let server_exited = self
                .lsp_server_process_channels
                .get_mut(&server_key)
                .is_some_and(|channel| !channel.is_running());
            if server_exited {
                log::warn!(
                    "Removing exited LSP server '{}' for root {lsp_root:?}",
                    config.id()
                );
                self.lsp_server_process_channels.remove(&server_key);
                self.record_server_failure(server_key.clone());
            }
            if let Some(channel) = self.lsp_server_process_channels.get(&server_key) {
                if notify_healthy_server && channel.is_initialized() {
                    channel.document_did_open(document.clone())?;
                }
            } else {
                self.start_server(&language, config, lsp_root, path)?;
            }
        }

        Ok(())
    }

    pub fn initialized(
        &mut self,
        language: Language,
        server_id: String,
        root: AbsolutePath,
        opened_documents: Vec<OpenDocument>,
    ) {
        let Some(server_key) = Self::server_key(&language, &server_id, root) else {
            return;
        };

        #[cfg(test)]
        self.lsp_server_initialized_args_history.push((
            format!("{}:{}", server_key.language_id, server_key.server_id),
            opened_documents
                .iter()
                .map(|document| document.path.clone())
                .collect(),
        ));

        self.lsp_server_process_channels
            .get_mut(&server_key)
            .map(|channel| {
                channel.initialized();
                channel.documents_did_open(opened_documents)
            });
    }

    pub fn shutdown(&mut self) {
        let channels = self
            .lsp_server_process_channels
            .drain()
            .map(|(_, channel)| channel)
            .collect::<Vec<_>>();
        for channel in &channels {
            channel
                .request_shutdown()
                .unwrap_or_else(|error| log::error!("{error:?}"));
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        for channel in channels {
            channel.wait_for_exit_until(deadline);
        }
    }

    /// Restarts the LSP servers responsible for `document`.
    pub fn restart(&mut self, document: OpenDocument) -> anyhow::Result<()> {
        let path = &document.path;
        let Some(language) = crate::config::from_path(path) else {
            return Ok(());
        };
        let keys = language
            .lsp_server_configs()
            .iter()
            .filter_map(|config| {
                Self::server_key(&language, config.id(), self.root_for_config(config, path))
            })
            .collect::<Vec<_>>();
        let channels = keys
            .into_iter()
            .filter_map(|key| self.lsp_server_process_channels.remove(&key))
            .collect::<Vec<_>>();

        for channel in &channels {
            channel
                .request_shutdown()
                .unwrap_or_else(|error| log::error!("{error:?}"));
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        for channel in channels {
            channel.wait_for_exit_until(deadline);
        }

        self.open_file(document)
    }

    #[cfg(test)]
    pub fn lsp_request_sent(&self, from_editor: &FromEditor) -> bool {
        self.history.get(from_editor.variant()) == Some(from_editor)
    }

    #[cfg(test)]
    pub fn lsp_server_initialized_args(&self) -> Option<(String, Vec<AbsolutePath>)> {
        self.lsp_server_initialized_args_history.last().cloned()
    }
}
