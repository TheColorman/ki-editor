mod buffer;
mod multibuffer;
use tracing_subscriber::filter::LevelFilter;

mod git;

mod alternator;
mod app;
pub mod char_index_range;
mod cli;
mod clipboard;
mod components;
pub mod config;
mod context;
mod divide_viewport;
mod edit;
mod editor_config;
mod embed;
mod env;
pub mod file_watcher;
mod format_path_list;
pub mod frontend;
#[cfg(test)]
mod generate_recipes;
mod grid;
pub mod history;
mod indent_query;
mod integration_event;
#[cfg(test)]
mod integration_test;
mod jj_conflict;
pub mod keymap;
mod keymap_override;
mod layout;
pub mod list;
mod lsp;
mod non_empty_extensions;
pub mod persistence;
mod position;
mod quickfix_list;
#[cfg(test)]
mod recipes;
mod rectangle;
mod render_flex_layout;
mod screen;
pub mod scripting;
mod search;
mod selection;
pub mod selection_mode;
pub mod selection_range;
pub mod soft_wrap;
pub mod style;
pub mod surround;
pub mod syntax_highlight;
#[cfg(test)]
mod test_app;
#[cfg(test)]
mod test_cli;
#[cfg(test)]
mod test_lsp;
#[cfg(test)]
mod test_search;
pub mod themes;
mod thread;
pub mod transformation;
pub mod ui_tree;
mod utils;
mod wakatime;
use std::{
    fs::File,
    io::Write,
    path::Path,
    rc::Rc,
    sync::{Arc, Mutex},
};

use anyhow::Context;
use frontend::crossterm::Crossterm;
use shared::absolute_path::AbsolutePath;

use app::App;

use crate::{app::AppMessage, cli::LogKind, config::AppConfig, persistence::Persistence};

pub fn main() {
    cli::cli().unwrap();
}

#[derive(Default)]
pub struct RunConfig {
    pub entry_path: Option<AbsolutePath>,
    pub working_directory: Option<AbsolutePath>,
}

const MAX_LOG_FILE_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Clone)]
struct BoundedLogFile {
    file: Arc<Mutex<File>>,
    max_bytes: u64,
}

impl BoundedLogFile {
    fn open(path: &Path, max_bytes: u64) -> anyhow::Result<Self> {
        Ok(Self {
            file: Arc::new(Mutex::new(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .read(true)
                    .open(path)?,
            )),
            max_bytes,
        })
    }

    fn write_event(&self, event: &[u8]) -> std::io::Result<()> {
        if event.len() as u64 > self.max_bytes {
            return Ok(());
        }

        let mut file = self.file.lock().unwrap();
        lock_file(&file)?;
        let result = (|| {
            if file.metadata()?.len() + event.len() as u64 > self.max_bytes {
                file.set_len(0)?;
            }
            file.write_all(event)
        })();
        let unlock_result = unlock_file(&file);
        result.and(unlock_result)
    }
}

struct BoundedLogWriter {
    log: BoundedLogFile,
    event: Vec<u8>,
}

impl Write for BoundedLogWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.event.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for BoundedLogWriter {
    fn drop(&mut self) {
        if let Err(error) = self.log.write_event(&self.event) {
            eprintln!("Failed to write Ki log: {error}");
        }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for BoundedLogFile {
    type Writer = BoundedLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        BoundedLogWriter {
            log: self.clone(),
            event: Vec::new(),
        }
    }
}

#[cfg(unix)]
fn lock_file(file: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn unlock_file(file: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn lock_file(_file: &File) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn unlock_file(_file: &File) -> std::io::Result<()> {
    Ok(())
}

fn init_logger() -> anyhow::Result<()> {
    use tracing_subscriber::prelude::*;

    fn open_log_file(log_kind: LogKind) -> anyhow::Result<BoundedLogFile> {
        BoundedLogFile::open(&log_kind.as_path()?, MAX_LOG_FILE_BYTES)
    }

    tracing_log::LogTracer::init()?;

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(open_log_file(LogKind::Default)?)
                .with_line_number(true)
                .with_ansi(false)
                .with_filter(
                    std::env::var("KI_LOG")
                        .ok()
                        .map(|value| {
                            value
                                .parse()
                                .unwrap_or_else(|error| panic!("Invalid KI_LOG value: {error}"))
                        })
                        .unwrap_or(LevelFilter::INFO),
                ),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(open_log_file(LogKind::Lsp)?)
                .with_line_number(true)
                .with_ansi(false)
                .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
                    metadata.target().starts_with("ki::lsp")
                })),
        )
        .try_init()?;

    Ok(())
}

pub fn run(config: RunConfig) -> anyhow::Result<()> {
    let _ = init_logger();
    std::fs::create_dir_all(grammar::cache_dir()).context("Failed to create cache_dir")?;
    let (sender, receiver) = crossbeam_channel::unbounded();
    let (priority_sender, priority_receiver) = crossbeam_channel::unbounded();
    let syntax_highlighter_sender = syntax_highlight::start_thread(sender.clone());

    let app = App::from_channel(
        Rc::new(Mutex::new(Crossterm::new())),
        config.working_directory.unwrap_or(".".try_into()?),
        sender,
        receiver,
        priority_sender.clone(),
        priority_receiver,
        Some(syntax_highlighter_sender),
        AppConfig::singleton().status_lines(),
        None, // No integration event sender
        true,
        true,
        false,
        Some(Persistence::load_or_default(
            grammar::cache_dir().join("data.json"),
        )),
    )?;

    std::thread::spawn(move || loop {
        let message = match crossterm::event::read() {
            Ok(event) => AppMessage::Event(event.into()),
            Err(err) => AppMessage::NotifyError(err),
        };

        let _ = priority_sender
            .send(message)
            .map_err(|err| log::info!("main::run::crossterm {err:#?}"));
    });

    app.run(config.entry_path)
        .map_err(|error| anyhow::anyhow!("screen.run {:?}", error))?;

    Ok(())
}

#[cfg(test)]
mod bounded_log_tests {
    use super::*;
    use tracing_subscriber::fmt::MakeWriter;

    #[test]
    fn log_file_is_truncated_before_exceeding_limit() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let path = tempdir.path().join("log");
        let log = BoundedLogFile::open(&path, 32)?;

        {
            let mut writer = log.make_writer();
            writer.write_all(b"aaaaaaaaaaaaaaaaaaaa")?;
        }
        {
            let mut writer = log.make_writer();
            writer.write_all(b"bbbbbbbbbbbbbbbbbbbb")?;
        }

        assert_eq!(std::fs::read(path)?, b"bbbbbbbbbbbbbbbbbbbb");
        Ok(())
    }

    #[test]
    fn oversized_log_event_is_discarded() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let path = tempdir.path().join("log");
        let log = BoundedLogFile::open(&path, 8)?;

        {
            let mut writer = log.make_writer();
            writer.write_all(b"too large")?;
        }

        assert!(std::fs::read(path)?.is_empty());
        Ok(())
    }
}
