use std::path::Path;

use anyhow::{bail, Context};
use serde::Deserialize;

const VALID_INTERVALS: &[&str] = &[
    "1m", "2m", "5m", "15m", "30m", "60m", "1h", "1d", "1wk", "1mo",
];

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub market: MarketConfig,
    #[serde(default)]
    pub paper: PaperConfig,
    #[serde(default)]
    pub fees: FeesConfig,
    #[serde(default)]
    pub sizing: SizingConfig,
    #[serde(default)]
    pub engine: EngineConfig,
    #[serde(default)]
    pub notifier: NotifierConfig,
    #[serde(default)]
    pub report: ReportConfig,
    #[serde(default)]
    pub social: SocialConfig,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub strategies: Vec<StrategyConfig>,
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("could not read config file {}", path.display()))?;
        let config: Config = toml::from_str(&contents)
            .with_context(|| format!("could not parse config file {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.paper.starting_cash_eur <= 0.0 {
            bail!("paper.starting_cash_eur must be positive");
        }
        if self.engine.max_history < 2 {
            bail!("engine.max_history must be at least 2");
        }
        if self.engine.tick_secs == 0 {
            bail!("engine.tick_secs must be at least 1");
        }
        if self.engine.log_heartbeat_secs == 0 {
            bail!("engine.log_heartbeat_secs must be at least 1");
        }
        if self.sizing.fraction <= 0.0 || self.sizing.fraction > 1.0 {
            bail!("sizing.fraction must be in (0, 1]");
        }
        if self.sizing.max_fraction <= 0.0 || self.sizing.max_fraction >= 1.0 {
            bail!("sizing.max_fraction must be in (0, 1); all-in is not allowed");
        }
        if self.sizing.fixed_quantity <= 0.0 {
            bail!("sizing.fixed_quantity must be positive");
        }
        if self.fees.commission_flat < 0.0
            || self.fees.commission_pct < 0.0
            || self.fees.slippage_bps < 0.0
        {
            bail!("fees values must not be negative");
        }
        if self.market.fx_fallback_rate <= 0.0 {
            bail!("market.fx_fallback_rate must be positive");
        }

        for strategy in &self.strategies {
            if strategy.symbols.is_empty() {
                bail!("strategy '{}' has no symbols", strategy.name);
            }
            validate_interval(&strategy.interval, &strategy.name)?;
        }

        if self.agent.enabled {
            if self.agent.min_interval_secs == 0 {
                bail!("agent.min_interval_secs must be at least 1");
            }
            if self.agent.retry_attempts == 0 {
                bail!("agent.retry_attempts must be at least 1");
            }
            if self.agent.failure_cooldown_secs == 0 {
                bail!("agent.failure_cooldown_secs must be at least 1");
            }
            validate_interval(&self.agent.interval, "agent")?;
            if self.agent_symbols().is_empty() {
                bail!("agent is enabled but no symbols are configured");
            }
        }

        if self.report.enabled {
            if self.report.html_path.trim().is_empty() {
                bail!("report.html_path must not be empty");
            }
            if self.report.history_path.trim().is_empty() {
                bail!("report.history_path must not be empty");
            }
            if self.report.max_history_points < 2 {
                bail!("report.max_history_points must be at least 2");
            }
        }

        if self.strategies.is_empty() && !self.agent.enabled {
            bail!("no signal sources configured: add [[strategies]] or enable [agent]");
        }

        Ok(())
    }

    pub fn agent_symbols(&self) -> Vec<String> {
        if let Some(symbols) = &self.agent.symbols {
            if !symbols.is_empty() {
                return symbols.clone();
            }
        }
        self.strategies
            .iter()
            .flat_map(|strategy| strategy.symbols.iter().cloned())
            .collect()
    }
}

fn validate_interval(interval: &str, owner: &str) -> anyhow::Result<()> {
    if !VALID_INTERVALS.contains(&interval) {
        bail!(
            "{} has unsupported interval '{}'; valid intervals: {}",
            owner,
            interval,
            VALID_INTERVALS.join(", ")
        );
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct MarketConfig {
    #[serde(default = "default_market_provider")]
    pub provider: String,
    #[serde(default = "default_market_base_url")]
    pub base_url: String,
    #[serde(default = "default_instrument_currency")]
    pub instrument_currency: String,
    #[serde(default = "default_quote_currency")]
    pub quote_currency: String,
    #[serde(default = "default_fx_fallback")]
    pub fx_fallback_rate: f64,
}

impl Default for MarketConfig {
    fn default() -> Self {
        Self {
            provider: default_market_provider(),
            base_url: default_market_base_url(),
            instrument_currency: default_instrument_currency(),
            quote_currency: default_quote_currency(),
            fx_fallback_rate: default_fx_fallback(),
        }
    }
}

fn default_market_provider() -> String {
    "yahoo".to_string()
}
fn default_market_base_url() -> String {
    "https://query1.finance.yahoo.com".to_string()
}
fn default_instrument_currency() -> String {
    "USD".to_string()
}
fn default_quote_currency() -> String {
    "EUR".to_string()
}
fn default_fx_fallback() -> f64 {
    0.92
}

#[derive(Debug, Deserialize)]
pub struct PaperConfig {
    #[serde(default = "default_starting_cash")]
    pub starting_cash_eur: f64,
    #[serde(default = "default_state_path")]
    pub state_path: String,
    #[serde(default = "default_allow_short")]
    pub allow_short: bool,
}

impl Default for PaperConfig {
    fn default() -> Self {
        Self {
            starting_cash_eur: default_starting_cash(),
            state_path: default_state_path(),
            allow_short: default_allow_short(),
        }
    }
}

fn default_starting_cash() -> f64 {
    100.0
}
fn default_state_path() -> String {
    "portfolio.json".to_string()
}
fn default_allow_short() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct FeesConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub commission_flat: f64,
    #[serde(default)]
    pub commission_pct: f64,
    #[serde(default = "default_slippage_bps")]
    pub slippage_bps: f64,
}

impl Default for FeesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            commission_flat: 0.0,
            commission_pct: 0.0,
            slippage_bps: default_slippage_bps(),
        }
    }
}

fn default_slippage_bps() -> f64 {
    5.0
}

#[derive(Debug, Deserialize)]
pub struct SizingConfig {
    #[serde(default = "default_sizing_mode")]
    pub mode: String,
    #[serde(default = "default_fraction")]
    pub fraction: f64,
    #[serde(default = "default_max_fraction")]
    pub max_fraction: f64,
    #[serde(default = "default_fixed_quantity")]
    pub fixed_quantity: f64,
    #[serde(default)]
    pub use_quantity_hint: bool,
}

impl Default for SizingConfig {
    fn default() -> Self {
        Self {
            mode: default_sizing_mode(),
            fraction: default_fraction(),
            max_fraction: default_max_fraction(),
            fixed_quantity: default_fixed_quantity(),
            use_quantity_hint: false,
        }
    }
}

fn default_sizing_mode() -> String {
    "equity_fraction".to_string()
}
fn default_fraction() -> f64 {
    0.25
}
fn default_max_fraction() -> f64 {
    0.25
}
fn default_fixed_quantity() -> f64 {
    1.0
}

#[derive(Debug, Deserialize)]
pub struct EngineConfig {
    #[serde(default = "default_tick_secs")]
    pub tick_secs: u64,
    #[serde(default = "default_max_history")]
    pub max_history: usize,
    #[serde(default = "default_true")]
    pub market_hours_only: bool,
    #[serde(default = "default_log_heartbeat")]
    pub log_heartbeat_secs: u64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            tick_secs: default_tick_secs(),
            max_history: default_max_history(),
            market_hours_only: true,
            log_heartbeat_secs: default_log_heartbeat(),
        }
    }
}

fn default_tick_secs() -> u64 {
    5
}
fn default_max_history() -> usize {
    300
}
fn default_log_heartbeat() -> u64 {
    60
}

#[derive(Debug, Deserialize)]
pub struct NotifierConfig {
    #[serde(default = "default_true")]
    pub console: bool,
    #[serde(default = "default_log_dir")]
    pub log_dir: String,
}

impl Default for NotifierConfig {
    fn default() -> Self {
        Self {
            console: true,
            log_dir: default_log_dir(),
        }
    }
}

fn default_log_dir() -> String {
    "logs".to_string()
}

#[derive(Debug, Deserialize)]
pub struct ReportConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_report_html_path")]
    pub html_path: String,
    #[serde(default = "default_report_history_path")]
    pub history_path: String,
    /// How often to refresh the HTML report. Zero refreshes only on trades.
    #[serde(default = "default_report_refresh")]
    pub refresh_secs: u64,
    #[serde(default = "default_report_history_points")]
    pub max_history_points: usize,
}

impl Default for ReportConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            html_path: default_report_html_path(),
            history_path: default_report_history_path(),
            refresh_secs: default_report_refresh(),
            max_history_points: default_report_history_points(),
        }
    }
}

fn default_report_html_path() -> String {
    "report.html".to_string()
}
fn default_report_history_path() -> String {
    "report-history.json".to_string()
}
fn default_report_refresh() -> u64 {
    60
}
fn default_report_history_points() -> usize {
    500
}

#[derive(Debug, Deserialize)]
pub struct SocialConfig {
    #[serde(default = "default_social_provider")]
    pub provider: String,
    #[serde(default)]
    pub accounts: Vec<String>,
    #[serde(default = "default_social_lookback")]
    pub lookback_hours: i64,
    #[serde(default = "default_social_refresh")]
    pub refresh_secs: u64,
}

impl Default for SocialConfig {
    fn default() -> Self {
        Self {
            provider: default_social_provider(),
            accounts: Vec::new(),
            lookback_hours: default_social_lookback(),
            refresh_secs: default_social_refresh(),
        }
    }
}

fn default_social_provider() -> String {
    "none".to_string()
}
fn default_social_lookback() -> i64 {
    24
}
fn default_social_refresh() -> u64 {
    1_800
}

#[derive(Debug, Deserialize)]
pub struct AgentConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_agent_base_url")]
    pub base_url: String,
    #[serde(default = "default_agent_model")]
    pub model: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_agent_min_interval")]
    pub min_interval_secs: u64,
    #[serde(default = "default_agent_timeout")]
    pub timeout_secs: u64,
    #[serde(default = "default_agent_retry")]
    pub retry_attempts: u32,
    #[serde(default = "default_agent_cooldown")]
    pub failure_cooldown_secs: u64,
    #[serde(default = "default_agent_interval")]
    pub interval: String,
    #[serde(default)]
    pub symbols: Option<Vec<String>>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_agent_base_url(),
            model: default_agent_model(),
            api_key: None,
            min_interval_secs: default_agent_min_interval(),
            timeout_secs: default_agent_timeout(),
            retry_attempts: default_agent_retry(),
            failure_cooldown_secs: default_agent_cooldown(),
            interval: default_agent_interval(),
            symbols: None,
        }
    }
}

fn default_agent_base_url() -> String {
    "http://localhost:8787/v1".to_string()
}
fn default_agent_model() -> String {
    "deepseek-v4.1-flash".to_string()
}
fn default_agent_min_interval() -> u64 {
    3_600
}
fn default_agent_timeout() -> u64 {
    60
}
fn default_agent_retry() -> u32 {
    3
}
fn default_agent_cooldown() -> u64 {
    60
}
fn default_agent_interval() -> String {
    "1d".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct StrategyConfig {
    pub name: String,
    pub symbols: Vec<String>,
    #[serde(default = "default_strategy_interval")]
    pub interval: String,
    #[serde(default = "empty_params")]
    pub params: toml::Value,
}

fn default_strategy_interval() -> String {
    "1d".to_string()
}
fn empty_params() -> toml::Value {
    toml::Value::Table(toml::map::Map::new())
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses_and_validates() {
        let config: Config = toml::from_str(include_str!("../../../config.example.toml"))
            .expect("example config should parse");
        config.validate().expect("example config should validate");
    }
}
