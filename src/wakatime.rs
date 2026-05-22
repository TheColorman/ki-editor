use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use shared::absolute_path::AbsolutePath;
use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(120);
const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Deserialize, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct WakaTimeConfig {
    enabled: bool,
    cli_path: String,
}

impl Default for WakaTimeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cli_path: "wakatime-cli".to_string(),
        }
    }
}

impl WakaTimeConfig {
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    fn cli_path(&self) -> &str {
        &self.cli_path
    }
}

pub struct WakaTime {
    config: WakaTimeConfig,
    last_heartbeat: Option<LastHeartbeat>,
}

struct LastHeartbeat {
    path: AbsolutePath,
    sent_at: Instant,
}

impl WakaTime {
    pub fn new(config: WakaTimeConfig) -> Self {
        Self {
            config,
            last_heartbeat: None,
        }
    }

    pub fn send_heartbeat(&mut self, path: AbsolutePath, is_write: bool) {
        if !path.as_ref().is_file() {
            return;
        }

        if !self.should_send(&path, is_write) {
            return;
        }

        self.last_heartbeat = Some(LastHeartbeat {
            path: path.clone(),
            sent_at: Instant::now(),
        });

        let cli_path = self.config.cli_path().to_string();
        let entity = path.display_absolute();
        let mut args = vec![
            "--entity".to_string(),
            entity,
            "--plugin".to_string(),
            format!("ki/{PLUGIN_VERSION} ki-wakatime/{PLUGIN_VERSION}"),
        ];
        if is_write {
            args.push("--write".to_string());
        }

        std::thread::spawn(move || {
            let output = Command::new(&cli_path)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output();

            match output {
                Ok(output) if output.status.success() => {}
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    log::warn!(
                        "wakatime-cli exited with status {}: {}",
                        output.status,
                        stderr.trim()
                    );
                }
                Err(error) => log::warn!("Failed to run wakatime-cli: {error}"),
            }
        });
    }

    fn should_send(&self, path: &AbsolutePath, is_write: bool) -> bool {
        if is_write {
            return true;
        }

        self.last_heartbeat
            .as_ref()
            .is_none_or(|last| last.path != *path || last.sent_at.elapsed() >= HEARTBEAT_INTERVAL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_heartbeats_are_never_rate_limited() {
        let path = AbsolutePath::try_from("/tmp/wakatime-test.rs".to_string()).unwrap();
        let mut wakatime = WakaTime::new(WakaTimeConfig::default());
        wakatime.last_heartbeat = Some(LastHeartbeat {
            path: path.clone(),
            sent_at: Instant::now(),
        });

        assert!(wakatime.should_send(&path, true));
    }

    #[test]
    fn repeated_non_write_heartbeats_are_rate_limited() {
        let path = AbsolutePath::try_from("/tmp/wakatime-test.rs".to_string()).unwrap();
        let mut wakatime = WakaTime::new(WakaTimeConfig::default());
        wakatime.last_heartbeat = Some(LastHeartbeat {
            path: path.clone(),
            sent_at: Instant::now(),
        });

        assert!(!wakatime.should_send(&path, false));
    }
}
