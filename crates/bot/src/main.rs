mod config;
mod runner;

use std::path::PathBuf;

use anyhow::Context;
use config::{Config, NotifierConfig};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_path = parse_args();
    let config = Config::load(&config_path)?;
    let _guard = init_tracing(&config.notifier)?;

    tracing::info!(config = %config_path.display(), "starting bye-watch");
    runner::run(config).await
}

fn parse_args() -> PathBuf {
    let mut args = std::env::args().skip(1);
    let mut path = PathBuf::from("config.toml");
    while let Some(argument) = args.next() {
        if argument == "--config" {
            if let Some(value) = args.next() {
                path = PathBuf::from(value);
            }
        } else if !argument.starts_with('-') {
            path = PathBuf::from(argument);
        }
    }
    path
}

fn init_tracing(config: &NotifierConfig) -> anyhow::Result<WorkerGuard> {
    std::fs::create_dir_all(&config.log_dir)
        .with_context(|| format!("could not create log directory {}", config.log_dir))?;

    let appender = tracing_appender::rolling::daily(&config.log_dir, "bye-watch.log");
    let (file_writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    if config.console {
        let console_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stdout);
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(file_writer)
            .with_ansi(false);
        tracing_subscriber::registry()
            .with(filter)
            .with(console_layer)
            .with(file_layer)
            .init();
    } else {
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(file_writer)
            .with_ansi(false);
        tracing_subscriber::registry()
            .with(filter)
            .with(file_layer)
            .init();
    }

    Ok(guard)
}
