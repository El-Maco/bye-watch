mod backtest;
mod config;
mod execution;
mod report;
mod runner;
mod setup;

use std::path::PathBuf;

use anyhow::{bail, Context};
use chrono::NaiveDate;
use config::{Config, NotifierConfig};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

enum Command {
    Live {
        config_path: PathBuf,
    },
    Backtest {
        config_path: PathBuf,
        args: backtest::BacktestArgs,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match parse_args()? {
        Command::Live { config_path } => {
            let config = Config::load(&config_path)?;
            let _guard = init_tracing(&config.notifier)?;
            tracing::info!(config = %config_path.display(), "starting bye-watch");
            runner::run(config).await
        }
        Command::Backtest { config_path, args } => {
            let config = Config::load(&config_path)?;
            let _guard = init_tracing(&config.notifier)?;
            tracing::info!(config = %config_path.display(), "starting bye-watch backtest");
            backtest::run(config, args).await
        }
    }
}

fn parse_args() -> anyhow::Result<Command> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut index = 0;
    let mut mode = "live";
    if let Some(first) = args.first() {
        if first == "backtest" || first == "live" {
            mode = if first == "backtest" {
                "backtest"
            } else {
                "live"
            };
            index = 1;
        }
    }

    let mut config_path = PathBuf::from("config.toml");
    let mut backtest_args = backtest::BacktestArgs::default();

    while index < args.len() {
        let argument = &args[index];
        index += 1;
        match argument.as_str() {
            "--config" => config_path = PathBuf::from(take(&args, &mut index, "--config")?),
            "--strategy" => {
                for name in take(&args, &mut index, "--strategy")?.split(',') {
                    let name = name.trim();
                    if !name.is_empty() {
                        backtest_args.strategies.push(name.to_string());
                    }
                }
            }
            "--symbol" => {
                for symbol in take(&args, &mut index, "--symbol")?.split(',') {
                    let symbol = symbol.trim();
                    if !symbol.is_empty() {
                        backtest_args.symbols.push(symbol.to_string());
                    }
                }
            }
            "--interval" => {
                backtest_args.interval = Some(take(&args, &mut index, "--interval")?.to_string())
            }
            "--from" => backtest_args.from = Some(parse_date(take(&args, &mut index, "--from")?)?),
            "--to" => backtest_args.to = Some(parse_date(take(&args, &mut index, "--to")?)?),
            "--cash" => {
                backtest_args.cash = Some(
                    take(&args, &mut index, "--cash")?
                        .parse()
                        .context("--cash must be a number")?,
                )
            }
            "--html" => {
                backtest_args.html_path = Some(take(&args, &mut index, "--html")?.to_string())
            }
            "--json" => {
                backtest_args.json_path = Some(take(&args, &mut index, "--json")?.to_string())
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            "backtest" => mode = "backtest",
            "live" => mode = "live",
            other if other.starts_with('-') => bail!("unknown argument: {other}"),
            other => config_path = PathBuf::from(other),
        }
    }

    if mode == "backtest" {
        Ok(Command::Backtest {
            config_path,
            args: backtest_args,
        })
    } else {
        Ok(Command::Live { config_path })
    }
}

fn take<'a>(args: &'a [String], index: &mut usize, flag: &str) -> anyhow::Result<&'a str> {
    let value = args
        .get(*index)
        .with_context(|| format!("{flag} requires a value"))?;
    *index += 1;
    Ok(value)
}

fn parse_date(value: &str) -> anyhow::Result<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .with_context(|| format!("invalid date '{value}', expected YYYY-MM-DD"))
}

fn print_usage() {
    println!(
        "bye-watch\n\
         \n\
         Usage:\n\
           bot [live] [--config PATH]\n\
           bot backtest [--config PATH] [--strategy NAME[,NAME]] [--symbol SYM[,SYM]]\n\
                        [--interval 1m|5m|15m|30m|1h|1d|1wk|1mo]\n\
                        [--from YYYY-MM-DD] [--to YYYY-MM-DD] [--cash EUR]\n\
                        [--html PATH] [--json PATH]\n\
         \n\
         Defaults: config.toml, all configured strategies, the strategy's symbols and\n\
         interval, and the full available history."
    );
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
