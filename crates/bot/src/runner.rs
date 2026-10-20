use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::bail;
use chrono::{DateTime, Datelike, Timelike, Utc};
use chrono_tz::America::New_York;
use engine::{
    decimal_from_f64, ActionReport, Broker, Candle, ConsoleNotifier, EquityFractionSizer,
    FeeConfig, FeeModel, FixedSizer, FxSource, MarketData, MarketKey, Notifier, OrderRequest,
    PaperBroker, Portfolio, PortfolioSummary, PortfolioView, PositionSizer, PositionSummary,
    PositionView, Report, Side, SignalIntent, SignalSource, SocialPost, SourceConfig,
    SourceRegistry, TickContext,
};
use market_yahoo::YahooClient;
use rust_decimal::Decimal;
use social::build_provider;

use crate::config::{AgentConfig, Config, EngineConfig};

struct SourceEntry {
    source: Box<dyn SignalSource>,
    symbols: Vec<String>,
    interval: String,
}

pub async fn run(config: Config) -> anyhow::Result<()> {
    if !config.market.provider.eq_ignore_ascii_case("yahoo") {
        tracing::warn!(
            provider = %config.market.provider,
            "only the yahoo market provider is implemented; using yahoo"
        );
    }

    let market = YahooClient::with_base_url(&config.market.base_url)?;

    let fees = FeeModel::from(FeeConfig {
        enabled: config.fees.enabled,
        commission_flat: decimal_from_f64(config.fees.commission_flat)?,
        commission_pct: decimal_from_f64(config.fees.commission_pct)?,
        slippage_bps: decimal_from_f64(config.fees.slippage_bps)?,
    });
    let max_fraction = decimal_from_f64(config.sizing.max_fraction)?;
    let use_quantity_hint = config.sizing.use_quantity_hint;

    let sizer: Box<dyn PositionSizer> = match config.sizing.mode.as_str() {
        "fixed" => Box::new(FixedSizer {
            quantity: decimal_from_f64(config.sizing.fixed_quantity)?,
            max_fraction,
            use_quantity_hint,
        }),
        "equity_fraction" => Box::new(EquityFractionSizer {
            fraction: decimal_from_f64(config.sizing.fraction)?,
            max_fraction,
            use_quantity_hint,
        }),
        other => bail!("unknown sizing.mode '{other}'"),
    };

    let starting_cash = decimal_from_f64(config.paper.starting_cash_eur)?;
    let state_path = Path::new(&config.paper.state_path);
    let mut broker: Box<dyn Broker> = Box::new(PaperBroker::load_or_new(
        state_path,
        starting_cash,
        fees,
        config.paper.allow_short,
    )?);

    let mut registry = SourceRegistry::new();
    strategy_ma_cross::register(&mut registry);
    strategy_rsi::register(&mut registry);
    strategy_pairs::register(&mut registry);
    agent::register(&mut registry);

    let mut sources: Vec<SourceEntry> = Vec::new();
    for strategy in &config.strategies {
        let source_config = SourceConfig {
            name: strategy.name.clone(),
            symbols: strategy.symbols.clone(),
            interval: strategy.interval.clone(),
            params: strategy.params.clone(),
        };
        let source = registry.build(&source_config)?;
        sources.push(SourceEntry {
            source,
            symbols: strategy.symbols.clone(),
            interval: strategy.interval.clone(),
        });
    }

    if config.agent.enabled {
        let symbols = config.agent_symbols();
        let source_config = SourceConfig {
            name: "llm_agent".to_string(),
            symbols: symbols.clone(),
            interval: config.agent.interval.clone(),
            params: agent_params(&config.agent),
        };
        let source = registry.build(&source_config)?;
        sources.push(SourceEntry {
            source,
            symbols,
            interval: config.agent.interval.clone(),
        });
    }

    let mut keys: Vec<MarketKey> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for entry in &sources {
        for symbol in &entry.symbols {
            if seen.insert((symbol.clone(), entry.interval.clone())) {
                keys.push(MarketKey::new(symbol, &entry.interval));
            }
        }
    }

    let social_provider = build_provider(&config.social.provider);
    let notifier: Box<dyn Notifier> = Box::new(ConsoleNotifier);

    tracing::info!(
        sources = sources.len(),
        markets = keys.len(),
        cash = %broker.portfolio().cash,
        sizing = %sizer.describe(),
        fees_enabled = config.fees.enabled,
        "bye-watch started"
    );

    let mut candles: HashMap<MarketKey, Vec<Candle>> = HashMap::new();
    let mut last_fetch: HashMap<MarketKey, Instant> = HashMap::new();
    let mut fx_rate = decimal_from_f64(config.market.fx_fallback_rate)?;
    let mut fx_last: Option<Instant> = None;
    let mut social_posts: Vec<SocialPost> = Vec::new();
    let mut social_last: Option<Instant> = None;
    let tick = Duration::from_secs(config.engine.tick_secs);
    let fx_refresh = Duration::from_secs(900);

    loop {
        let now = Utc::now();
        let market_open = is_us_market_open(now);

        for key in &keys {
            if !should_fetch(key, &last_fetch, market_open, &config.engine) {
                continue;
            }
            match market
                .candles(&key.symbol, &key.interval, config.engine.max_history)
                .await
            {
                Ok(series) => {
                    tracing::debug!(
                        symbol = %key.symbol,
                        interval = %key.interval,
                        candles = series.len(),
                        "fetched candles"
                    );
                    candles.insert(key.clone(), series);
                    last_fetch.insert(key.clone(), Instant::now());
                }
                Err(err) => {
                    tracing::warn!(
                        symbol = %key.symbol,
                        interval = %key.interval,
                        error = %err,
                        "failed to fetch market data"
                    );
                    last_fetch.insert(key.clone(), Instant::now());
                }
            }
        }

        if fx_last
            .map(|instant| instant.elapsed() >= fx_refresh)
            .unwrap_or(true)
        {
            match market
                .rate(
                    &config.market.instrument_currency,
                    &config.market.quote_currency,
                )
                .await
            {
                Ok(rate) if rate > Decimal::ZERO => {
                    fx_rate = rate;
                    fx_last = Some(Instant::now());
                    tracing::debug!(rate = %rate, "updated FX rate");
                }
                Ok(_) => {
                    tracing::warn!("FX rate was not positive; keeping {}", fx_rate);
                    fx_last = Some(Instant::now());
                }
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        fallback = %fx_rate,
                        "failed to fetch FX rate; using fallback"
                    );
                    fx_last = Some(Instant::now());
                }
            }
        }

        if social_last
            .map(|instant| instant.elapsed() >= Duration::from_secs(config.social.refresh_secs))
            .unwrap_or(true)
        {
            let since = now - chrono::Duration::hours(config.social.lookback_hours.max(1));
            match social_provider.fetch(&config.social.accounts, since).await {
                Ok(posts) => {
                    if !posts.is_empty() {
                        tracing::debug!(posts = posts.len(), "fetched social posts");
                    }
                    social_posts = posts;
                    social_last = Some(Instant::now());
                }
                Err(err) => {
                    tracing::warn!(error = %err, "social fetch failed; continuing");
                    social_last = Some(Instant::now());
                }
            }
        }

        let mut latest: HashMap<String, (i64, f64)> = HashMap::new();
        for key in &keys {
            if let Some(series) = candles.get(key) {
                if let Some(last) = series.last() {
                    let entry = latest
                        .entry(key.symbol.clone())
                        .or_insert((last.timestamp, last.close));
                    if last.timestamp > entry.0 {
                        *entry = (last.timestamp, last.close);
                    }
                }
            }
        }

        let mut prices: HashMap<String, Decimal> = HashMap::new();
        for (symbol, (_, close)) in latest {
            match decimal_from_f64(close) {
                Ok(price) => {
                    prices.insert(symbol, price * fx_rate);
                }
                Err(err) => tracing::warn!(symbol, error = %err, "could not convert price"),
            }
        }

        let ctx = TickContext {
            now,
            candles: candles.clone(),
            prices: prices.clone(),
            portfolio: portfolio_view(broker.portfolio(), &prices),
            social: social_posts.clone(),
        };

        let mut intents: Vec<SignalIntent> = Vec::new();
        for entry in &mut sources {
            if !should_evaluate(&entry.interval, market_open, &config.engine) {
                continue;
            }
            let produced = entry.source.on_tick(&ctx).await;
            if !produced.is_empty() {
                tracing::debug!(
                    source = entry.source.name(),
                    count = produced.len(),
                    "source produced signals"
                );
            }
            intents.extend(produced);
        }

        // Confidence-ranked aggregation: one winning intent per symbol.
        let mut best: HashMap<String, SignalIntent> = HashMap::new();
        for intent in intents {
            let replace = match best.get(&intent.symbol) {
                Some(existing) => intent.confidence > existing.confidence,
                None => true,
            };
            if replace {
                best.insert(intent.symbol.clone(), intent);
            }
        }

        let mut actions: Vec<ActionReport> = Vec::new();
        let mut skipped: Vec<String> = Vec::new();

        for intent in best.into_values() {
            let price = match prices.get(&intent.symbol).copied() {
                Some(price) if price > Decimal::ZERO => price,
                _ => {
                    skipped.push(format!("{}: no price available", intent.symbol));
                    continue;
                }
            };

            let snapshot = portfolio_view(broker.portfolio(), &prices);
            let quantity = match sizer.size(&intent, &snapshot, &prices) {
                Ok(quantity) => quantity,
                Err(err) => {
                    skipped.push(format!("{}: sizing failed: {err}", intent.symbol));
                    continue;
                }
            };

            let quantity = if intent.direction == Side::Buy && !intent.close_position {
                let cash = broker.portfolio().cash;
                quantity.min((cash / price).round_dp(6))
            } else {
                quantity
            };

            if quantity <= Decimal::ZERO {
                skipped.push(format!(
                    "{}: no size (close_position={}, direction={})",
                    intent.symbol,
                    intent.close_position,
                    intent.direction.as_str()
                ));
                continue;
            }

            let order = OrderRequest {
                symbol: intent.symbol.clone(),
                side: intent.direction,
                quantity,
                reference_price: price,
                reason: intent.reason.clone(),
                confidence: intent.confidence,
                source: intent.source.clone(),
            };

            match broker.place_order(&order).await {
                Ok(fill) => actions.push(ActionReport {
                    timestamp: fill.timestamp,
                    symbol: fill.symbol,
                    side: fill.side,
                    quantity: fill.quantity,
                    price: fill.price,
                    fees: fill.fees,
                    confidence: fill.confidence,
                    source: fill.source,
                    reason: fill.reason,
                }),
                Err(err) => skipped.push(format!("{}: order rejected: {err}", order.symbol)),
            }
        }

        if !actions.is_empty() {
            broker.persist(state_path)?;
            let report = Report {
                timestamp: now,
                actions,
                skipped,
                portfolio: portfolio_summary(broker.portfolio(), &prices),
            };
            notifier.notify(&report)?;
        } else {
            for line in skipped {
                tracing::debug!("{line}");
            }
        }

        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(err) = result {
                    tracing::warn!(error = %err, "failed to listen for shutdown signal");
                }
                tracing::info!("shutdown requested");
                break;
            }
            _ = tokio::time::sleep(tick) => {}
        }
    }

    broker.persist(state_path)?;
    tracing::info!("bye-watch stopped; portfolio saved");
    Ok(())
}

fn portfolio_view(portfolio: &Portfolio, prices: &HashMap<String, Decimal>) -> PortfolioView {
    let mut positions = HashMap::new();
    for (symbol, position) in &portfolio.positions {
        positions.insert(
            symbol.clone(),
            PositionView {
                quantity: position.quantity,
                avg_entry_price: position.avg_entry_price,
            },
        );
    }
    PortfolioView {
        cash: portfolio.cash,
        equity: portfolio.equity(prices),
        positions,
    }
}

fn portfolio_summary(portfolio: &Portfolio, prices: &HashMap<String, Decimal>) -> PortfolioSummary {
    let mut open_positions: Vec<PositionSummary> = portfolio
        .positions
        .values()
        .map(|position| {
            let price = prices
                .get(&position.symbol)
                .copied()
                .unwrap_or(position.avg_entry_price);
            PositionSummary {
                symbol: position.symbol.clone(),
                quantity: position.quantity,
                avg_entry_price: position.avg_entry_price,
                market_value: position.quantity * price,
                unrealized_pnl: position.unrealized_pnl(price),
            }
        })
        .collect();
    open_positions.sort_by(|left, right| left.symbol.cmp(&right.symbol));

    PortfolioSummary {
        cash: portfolio.cash,
        equity: portfolio.equity(prices),
        net_pnl: portfolio.net_pnl(prices),
        gross_pnl: portfolio.gross_pnl(prices),
        realized_pnl: portfolio.realized_pnl,
        unrealized_pnl: portfolio.unrealized_pnl(prices),
        fees_paid: portfolio.fees_paid,
        slippage_paid: portfolio.slippage_paid,
        open_positions,
    }
}

fn agent_params(agent: &AgentConfig) -> toml::Value {
    let mut table = toml::map::Map::new();
    table.insert(
        "base_url".to_string(),
        toml::Value::String(agent.base_url.clone()),
    );
    table.insert(
        "model".to_string(),
        toml::Value::String(agent.model.clone()),
    );
    if let Some(api_key) = &agent.api_key {
        table.insert("api_key".to_string(), toml::Value::String(api_key.clone()));
    }
    table.insert(
        "min_interval_secs".to_string(),
        toml::Value::Integer(agent.min_interval_secs as i64),
    );
    table.insert(
        "timeout_secs".to_string(),
        toml::Value::Integer(agent.timeout_secs as i64),
    );
    toml::Value::Table(table)
}

fn is_us_market_open(now: DateTime<Utc>) -> bool {
    let new_york = now.with_timezone(&New_York);
    if matches!(
        new_york.weekday(),
        chrono::Weekday::Sat | chrono::Weekday::Sun
    ) {
        return false;
    }
    let minutes = new_york.hour() * 60 + new_york.minute();
    (9 * 60 + 30..16 * 60).contains(&minutes)
}

fn is_long_interval(interval: &str) -> bool {
    matches!(interval, "1d" | "1wk" | "1mo")
}

fn interval_seconds(interval: &str) -> u64 {
    match interval {
        "1m" => 60,
        "2m" => 120,
        "5m" => 300,
        "15m" => 900,
        "30m" => 1_800,
        "60m" | "1h" => 3_600,
        "1d" => 86_400,
        "1wk" => 604_800,
        "1mo" => 2_592_000,
        _ => 60,
    }
}

fn should_fetch(
    key: &MarketKey,
    last_fetch: &HashMap<MarketKey, Instant>,
    market_open: bool,
    engine: &EngineConfig,
) -> bool {
    let elapsed = last_fetch.get(key).map(|instant| instant.elapsed());
    if is_long_interval(&key.interval) {
        if market_open && elapsed.is_some() {
            return false;
        }
        return elapsed
            .map(|elapsed| elapsed >= Duration::from_secs(3_600))
            .unwrap_or(true);
    }
    if engine.market_hours_only && !market_open {
        return false;
    }
    elapsed
        .map(|elapsed| elapsed >= Duration::from_secs(interval_seconds(&key.interval)))
        .unwrap_or(true)
}

fn should_evaluate(interval: &str, market_open: bool, engine: &EngineConfig) -> bool {
    if is_long_interval(interval) {
        return !market_open || !engine.market_hours_only;
    }
    if !engine.market_hours_only {
        return true;
    }
    market_open
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals_are_mapped() {
        assert_eq!(interval_seconds("1m"), 60);
        assert_eq!(interval_seconds("1h"), 3_600);
        assert_eq!(interval_seconds("1d"), 86_400);
    }

    #[test]
    fn long_intervals_are_not_fetched_twice_during_market_hours() {
        let engine = EngineConfig::default();
        let key = MarketKey::new("KO", "1d");
        let mut last_fetch = HashMap::new();
        last_fetch.insert(key.clone(), Instant::now());
        assert!(!should_fetch(&key, &last_fetch, true, &engine));
        assert!(should_fetch(&key, &HashMap::new(), true, &engine));
    }

    #[test]
    fn market_hours_gate_intraday_and_daily_evaluation() {
        let engine = EngineConfig::default();
        assert!(should_evaluate("1m", true, &engine));
        assert!(!should_evaluate("1m", false, &engine));
        assert!(!should_evaluate("1d", true, &engine));
        assert!(should_evaluate("1d", false, &engine));
    }
}
