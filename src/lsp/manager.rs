use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::app::AppMessage;
use crate::lsp::server_config::resolve_configured_path;

use super::process::{FromEditor, LspServerProcessChannel, OpenDocument};
use shared::{
    absolute_path::AbsolutePath,
    language::{Language, LspServerConfig},
};

const LSP_RESTART_BASE_DELAY: Duration = Duration::from_millis(250);
const LSP_RESTART_MAX_DELAY: Duration = Duration::from_secs(30);
const LSP_STABLE_UPTIME: Duration = Duration::from_secs(30);

fn is_package_workspace_root(path: &Path) -> bool {
    [
        "pnpm-workspace.yaml",
        "pnpm-workspace.yml",
        "pnpm-lock.yaml",
        "yarn.lock",
        "package-lock.json",
        "bun.lockb",
        "bun.lock",
    ]
    .into_iter()
    .any(|marker| path.join(marker).exists())
}

fn is_java_workspace_root(path: &Path) -> bool {
    [
        "settings.gradle",
        "settings.gradle.kts",
        "gradlew",
        "gradlew.bat",
        "mvnw",
        "mvnw.cmd",
    ]
    .into_iter()
    .any(|marker| path.join(marker).exists())
        || path.join(".mvn").is_dir()
}

fn is_java_project_root(path: &Path) -> bool {
    ["pom.xml", "build.gradle", "build.gradle.kts"]
        .into_iter()
        .any(|marker| path.join(marker).exists())
}

fn java_lsp_root(file_parent: &Path, boundary: &Path) -> PathBuf {
    let stop_at_boundary = file_parent.starts_with(boundary);
    let mut nearest_project = None;

    for ancestor in file_parent.ancestors() {
        if is_java_workspace_root(ancestor) {
            return ancestor.to_path_buf();
        }
        if nearest_project.is_none() && is_java_project_root(ancestor) {
            nearest_project = Some(ancestor.to_path_buf());
        }
        if stop_at_boundary && ancestor == boundary {
            break;
        }
    }

    nearest_project.unwrap_or_else(|| file_parent.to_path_buf())
}

fn contains_file_with_extension(path: &Path, extensions: &[&str]) -> bool {
    std::fs::read_dir(path).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    extensions
                        .iter()
                        .any(|candidate| extension.eq_ignore_ascii_case(candidate))
                })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
struct LspServerKey {
    language_id: String,
    server_id: String,
    root: AbsolutePath,
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
        let root = self.lsp_root_for_path(&language, path);
        language.lsp_server_configs().into_iter().any(|config| {
            Self::server_key(&language, config.id(), root.clone()).is_some_and(|key| {
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
        self.warn_missing_configured_lsp_paths(&root, &server_id, &config);
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

    pub(crate) fn lsp_root_for_path(
        &self,
        language: &Language,
        path: &AbsolutePath,
    ) -> AbsolutePath {
        let file_parent = path.as_ref().parent().unwrap_or(path.as_ref());
        let boundary = self.current_working_directory.as_ref();
        let is_java = language.id().is_some_and(|id| id.to_string() == "java");
        if is_java {
            let root = java_lsp_root(file_parent, boundary);
            let root = root.canonicalize().unwrap_or(root);
            return root
                .try_into()
                .expect("Java root derived from an absolute file path must be absolute");
        }
        let mut nearest_package_root = None::<PathBuf>;
        let mut nearest_csharp_project = None::<PathBuf>;
        let is_csharp = language.tree_sitter_grammar_id().as_deref() == Some("c_sharp");

        for ancestor in file_parent.ancestors() {
            if is_csharp {
                if contains_file_with_extension(ancestor, &["sln", "slnx"]) {
                    return ancestor
                        .try_into()
                        .unwrap_or_else(|_| self.current_working_directory.clone());
                }
                if nearest_csharp_project.is_none()
                    && contains_file_with_extension(ancestor, &["csproj"])
                {
                    nearest_csharp_project = Some(ancestor.to_path_buf());
                }
            }
            if is_package_workspace_root(ancestor) {
                if is_csharp {
                    nearest_package_root.get_or_insert_with(|| ancestor.to_path_buf());
                } else {
                    return ancestor
                        .try_into()
                        .unwrap_or_else(|_| self.current_working_directory.clone());
                }
            }
            if nearest_package_root.is_none() && ancestor.join("package.json").exists() {
                nearest_package_root = Some(ancestor.to_path_buf());
            }
            if ancestor == boundary {
                break;
            }
        }

        nearest_csharp_project
            .or(nearest_package_root)
            .and_then(|path| path.as_path().try_into().ok())
            .unwrap_or_else(|| self.current_working_directory.clone())
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

    fn warn_missing_configured_lsp_paths(
        &self,
        root: &AbsolutePath,
        server_id: &str,
        config: &shared::language::LspServerConfig,
    ) {
        let Some(options) = config.initialization_options() else {
            return;
        };

        if let Some(tsdk) = options
            .pointer("/typescript/tsdk")
            .or_else(|| options.pointer("/typescript.tsdk"))
            .and_then(|value| value.as_str())
        {
            let Some(resolved) = resolve_configured_path(root, tsdk) else {
                log::warn!(
                    "Unable to resolve configured TypeScript SDK placeholder {tsdk:?} for LSP server '{server_id}'"
                );
                return;
            };
            if !resolved.exists() {
                log::warn!(
                    "Configured LSP path for server '{server_id}' does not exist: typescript tsdk {tsdk:?} resolved to {}. If the server is installed/configured through Nix or another package manager, override this path in Ki config.",
                    resolved.display()
                );
            }
        }

        if let Some(plugins) = options
            .pointer("/vtsls/tsserver/globalPlugins")
            .and_then(|value| value.as_array())
        {
            for plugin in plugins {
                let Some(location) = plugin.get("location").and_then(|value| value.as_str()) else {
                    continue;
                };
                let Some(resolved) = resolve_configured_path(root, location) else {
                    let name = plugin
                        .get("name")
                        .and_then(|value| value.as_str())
                        .unwrap_or("<unknown plugin>");
                    log::warn!(
                        "Unable to resolve configured LSP plugin placeholder for server '{server_id}': plugin {name:?} location {location:?}. If the plugin is provided by Nix or another package manager, override this location in Ki config."
                    );
                    continue;
                };
                if !resolved.exists() {
                    let name = plugin
                        .get("name")
                        .and_then(|value| value.as_str())
                        .unwrap_or("<unknown plugin>");
                    log::warn!(
                        "Configured LSP plugin path for server '{server_id}' does not exist: plugin {name:?} location {location:?} resolved to {}. If the plugin is provided by Nix or another package manager, override this location in Ki config.",
                        resolved.display()
                    );
                }
            }
        }
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
        let root = self.lsp_root_for_path(&language, path);
        let configs = if Self::is_lifecycle_message(from_editor) {
            configs
        } else {
            configs
                .into_iter()
                .filter(|config| config.primary())
                .collect()
        };

        let mut errors = Vec::new();
        for config in configs {
            let Some(key) = Self::server_key(&language, config.id(), root.clone()) else {
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
            let lsp_root = self.lsp_root_for_path(&language, path);
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

    /// Restarts the LSP servers responsible for `path`.
    pub fn restart(&mut self, path: &AbsolutePath) -> anyhow::Result<()> {
        let Some(language) = crate::config::from_path(path) else {
            return Ok(());
        };
        let Some(language_id) = language.id() else {
            return Ok(());
        };
        let language_id = language_id.to_string();
        let root = self.lsp_root_for_path(&language, path);
        let keys = self
            .lsp_server_process_channels
            .keys()
            .filter(|key| key.language_id == language_id && key.root == root)
            .cloned()
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

        self.ensure_file_server(OpenDocument {
            path: path.clone(),
            version: 0,
            content: String::new(),
        })
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
