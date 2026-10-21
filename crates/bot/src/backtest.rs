use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use anyhow::{bail, Context};
use chrono::{DateTime, NaiveDate, Utc};
use engine::{
    decimal_from_f64, Broker, Candle, FxSource, MarketData, PaperBroker, Portfolio, Side,
    SignalSource, SourceConfig, SourceRegistry, TickContext, Trade,
};
use market_yahoo::YahooClient;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::Serialize;

use crate::config::{Config, StrategyConfig};
use crate::execution::{execute_intents, portfolio_view};
use crate::report::{self, EquityPoint};
use crate::setup::{build_fee_model, build_registry, build_sizer};

#[derive(Debug, Clone, Default)]
pub struct BacktestArgs {
    pub strategies: Vec<String>,
    pub symbols: Vec<String>,
    pub interval: Option<String>,
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub cash: Option<f64>,
    pub html_path: Option<String>,
    pub json_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PricePoint {
    pub timestamp: DateTime<Utc>,
    pub close: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BacktestResult {
    pub strategy: String,
    pub symbols: Vec<String>,
    pub interval: String,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub starting_cash: Decimal,
    pub final_equity: Decimal,
    pub net_pnl: Decimal,
    pub gross_pnl: Decimal,
    pub return_pct: Decimal,
    pub max_drawdown_pct: Decimal,
    pub trades: usize,
    pub closed_trades: usize,
    pub win_rate_pct: Decimal,
    pub profit_factor: Option<Decimal>,
    pub benchmark_return_pct: Decimal,
    pub equity_curve: Vec<EquityPoint>,
    pub price_series: HashMap<String, Vec<PricePoint>>,
    pub final_prices: HashMap<String, Decimal>,
    pub portfolio: Portfolio,
}

pub async fn run(config: Config, args: BacktestArgs) -> anyhow::Result<()> {
    if let (Some(from), Some(to)) = (args.from, args.to) {
        if from > to {
            bail!("--from must not be after --to");
        }
    }

    let market = YahooClient::with_base_url(&config.market.base_url)?;
    let registry = build_registry();
    let selected: Vec<&StrategyConfig> = config
        .strategies
        .iter()
        .filter(|strategy| {
            args.strategies.is_empty() || args.strategies.iter().any(|name| name == &strategy.name)
        })
        .collect();

    if selected.is_empty() {
        let available = config
            .strategies
            .iter()
            .map(|strategy| strategy.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        bail!("no matching strategies; available: {available}");
    }

    let fx_rate = match market
        .rate(
            &config.market.instrument_currency,
            &config.market.quote_currency,
        )
        .await
    {
        Ok(rate) if rate > Decimal::ZERO => rate,
        Ok(_) => {
            tracing::warn!("FX rate was not positive; using fallback");
            decimal_from_f64(config.market.fx_fallback_rate)?
        }
        Err(error) => {
            tracing::warn!(error = %error, "FX lookup failed; using fallback rate");
            decimal_from_f64(config.market.fx_fallback_rate)?
        }
    };

    let mut results = Vec::new();
    for strategy in selected {
        tracing::info!(strategy = %strategy.name, "running strategy backtest");
        match run_one(&config, &market, &registry, strategy, &args, fx_rate).await {
            Ok(result) => results.push(result),
            Err(error) => {
                tracing::warn!(strategy = %strategy.name, error = %error, "backtest failed")
            }
        }
    }

    if results.is_empty() {
        bail!("all backtests failed");
    }

    print_summary(&results);

    let html_path = args
        .html_path
        .as_deref()
        .unwrap_or(&config.backtest.html_path);
    let json_path = args
        .json_path
        .as_deref()
        .unwrap_or(&config.backtest.json_path);
    write_json(Path::new(json_path), &results)?;
    write_html(Path::new(html_path), &config, &results)?;
    Ok(())
}

async fn run_one(
    config: &Config,
    market: &YahooClient,
    registry: &SourceRegistry,
    strategy: &StrategyConfig,
    args: &BacktestArgs,
    fx_rate: Decimal,
) -> anyhow::Result<BacktestResult> {
    let symbols = if args.symbols.is_empty() {
        strategy.symbols.clone()
    } else {
        args.symbols.clone()
    };
    if symbols.is_empty() {
        bail!("strategy '{}' has no symbols", strategy.name);
    }

    let interval = args
        .interval
        .clone()
        .unwrap_or_else(|| strategy.interval.clone());
    let to = args
        .to
        .map(|date| date.and_hms_opt(23, 59, 59).expect("end of day").and_utc())
        .unwrap_or_else(Utc::now);
    let from = match args.from {
        Some(date) => Some(date.and_hms_opt(0, 0, 0).expect("midnight").and_utc()),
        None => default_history_start(&interval, to),
    };

    if let (Some(from), Some(max_days)) = (from, max_intraday_days(&interval)) {
        let earliest = to - chrono::Duration::days(max_days);
        if from < earliest {
            bail!(
                "interval {interval} only supports {max_days} days of history; use --from on or after {} or a daily interval",
                earliest.format("%Y-%m-%d")
            );
        }
    }

    let mut history: HashMap<String, Vec<Candle>> = HashMap::new();
    for symbol in &symbols {
        let candles = market.history(symbol, &interval, from, Some(to)).await?;
        if candles.is_empty() {
            bail!("no history for {symbol} at {interval}");
        }
        tracing::info!(
            symbol = %symbol,
            interval = %interval,
            candles = candles.len(),
            "loaded history"
        );
        history.insert(symbol.clone(), candles);
    }

    let source_config = SourceConfig {
        name: strategy.name.clone(),
        symbols: symbols.clone(),
        interval: interval.clone(),
        params: strategy.params.clone(),
    };
    let mut source = registry.build(&source_config)?;

    let starting_cash = match args.cash {
        Some(cash) => decimal_from_f64(cash)?,
        None => decimal_from_f64(config.paper.starting_cash_eur)?,
    };

    simulate(
        config,
        source.as_mut(),
        &strategy.name,
        &symbols,
        &interval,
        &history,
        starting_cash,
        fx_rate,
        from,
        Some(to),
    )
    .await
}

fn default_history_start(interval: &str, to: DateTime<Utc>) -> Option<DateTime<Utc>> {
    max_intraday_days(interval).map(|days| to - chrono::Duration::days(days))
}

fn max_intraday_days(interval: &str) -> Option<i64> {
    match interval {
        "1m" => Some(7),
        "2m" | "5m" | "15m" | "30m" | "90m" => Some(60),
        "60m" | "1h" => Some(730),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
async fn simulate(
    config: &Config,
    source: &mut dyn SignalSource,
    strategy_name: &str,
    symbols: &[String],
    interval: &str,
    history: &HashMap<String, Vec<Candle>>,
    starting_cash: Decimal,
    fx_rate: Decimal,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> anyhow::Result<BacktestResult> {
    let mut timeline = BTreeSet::new();
    for candles in history.values() {
        for candle in candles {
            timeline.insert(candle.timestamp);
        }
    }

    let fees = build_fee_model(config)?;
    let sizer = build_sizer(config)?;
    let mut broker = PaperBroker::new(
        Portfolio::new(starting_cash),
        fees,
        config.paper.allow_short,
    );
    let max_history = config.engine.max_history;
    let mut equity_curve = Vec::new();
    let mut pending = Vec::new();
    let mut last_close_prices = HashMap::new();
    let mut step_count = 0usize;

    for timestamp in timeline {
        let now = DateTime::<Utc>::from_timestamp(timestamp, 0).unwrap_or_else(Utc::now);

        let mut open_prices = HashMap::new();
        let mut close_prices = HashMap::new();
        let mut ctx_candles = HashMap::new();

        for symbol in symbols {
            let candles = match history.get(symbol) {
                Some(candles) => candles,
                None => continue,
            };
            let end = candles.partition_point(|candle| candle.timestamp <= timestamp);
            if end == 0 {
                continue;
            }
            let last = &candles[end - 1];
            let start = end.saturating_sub(max_history);
            ctx_candles.insert(
                engine::MarketKey::new(symbol, interval),
                candles[start..end].to_vec(),
            );
            open_prices.insert(symbol.clone(), decimal_from_f64(last.open)? * fx_rate);
            close_prices.insert(symbol.clone(), decimal_from_f64(last.close)? * fx_rate);
        }

        if close_prices.len() != symbols.len() {
            continue;
        }

        // Signals are generated from closed candles. They fill at the next
        // candle's open, which avoids using the close we just observed.
        let (actions, _skipped) = execute_intents(
            &mut broker,
            sizer.as_ref(),
            std::mem::take(&mut pending),
            &open_prices,
            now,
        )
        .await;
        if !actions.is_empty() {
            tracing::debug!(
                timestamp = %now,
                trades = actions.len(),
                "backtest orders filled"
            );
        }

        let ctx = TickContext {
            now,
            candles: ctx_candles,
            prices: close_prices.clone(),
            portfolio: portfolio_view(broker.portfolio(), &close_prices),
            social: Vec::new(),
        };

        pending = source.on_tick(&ctx).await;

        let equity = broker.portfolio().equity(&close_prices);
        equity_curve.push(EquityPoint {
            timestamp: now,
            equity,
            cash: broker.portfolio().cash,
            net_pnl: broker.portfolio().net_pnl(&close_prices),
        });
        last_close_prices = close_prices;
        step_count += 1;
        if step_count.is_multiple_of(5_000) {
            tracing::info!(steps = step_count, equity = %equity, "backtest progress");
        }
    }

    if equity_curve.is_empty() {
        bail!("no overlapping candles for {}", symbols.join(", "));
    }

    let portfolio = broker.portfolio().clone();
    let final_equity = portfolio.equity(&last_close_prices);
    let net_pnl = final_equity - starting_cash;
    let gross_pnl = portfolio.gross_pnl(&last_close_prices);
    let return_pct = report::percentage(net_pnl, starting_cash);
    let max_drawdown_pct = max_drawdown(&equity_curve);
    let (closed_trades, win_rate_pct, profit_factor) = trade_stats(&portfolio.trades);
    let benchmark_return_pct = benchmark_return(history, symbols);
    let equity_curve = downsample(equity_curve, config.backtest.max_curve_points);
    let fx = fx_rate.to_f64().unwrap_or(1.0);
    let mut price_series = HashMap::new();
    for symbol in symbols {
        if let Some(candles) = history.get(symbol) {
            let points = candles
                .iter()
                .map(|candle| PricePoint {
                    timestamp: DateTime::<Utc>::from_timestamp(candle.timestamp, 0)
                        .unwrap_or_else(Utc::now),
                    close: candle.close * fx,
                })
                .collect();
            price_series.insert(
                symbol.clone(),
                downsample_prices(points, config.backtest.max_curve_points),
            );
        }
    }

    Ok(BacktestResult {
        strategy: strategy_name.to_string(),
        symbols: symbols.to_vec(),
        interval: interval.to_string(),
        from,
        to,
        starting_cash,
        final_equity,
        net_pnl,
        gross_pnl,
        return_pct,
        max_drawdown_pct,
        trades: portfolio.trades.len(),
        closed_trades,
        win_rate_pct,
        profit_factor,
        benchmark_return_pct,
        equity_curve,
        price_series,
        final_prices: last_close_prices,
        portfolio,
    })
}

fn trade_stats(trades: &[Trade]) -> (usize, Decimal, Option<Decimal>) {
    let closed: Vec<&Trade> = trades
        .iter()
        .filter(|trade| !trade.realized_pnl.is_zero())
        .collect();
    if closed.is_empty() {
        return (0, Decimal::ZERO, None);
    }

    let wins = closed
        .iter()
        .filter(|trade| trade.realized_pnl > Decimal::ZERO)
        .count();
    let win_rate = Decimal::from(wins) / Decimal::from(closed.len()) * Decimal::from(100);

    let gross_profit: Decimal = closed
        .iter()
        .filter(|trade| trade.realized_pnl > Decimal::ZERO)
        .map(|trade| trade.realized_pnl)
        .sum();
    let gross_loss: Decimal = closed
        .iter()
        .filter(|trade| trade.realized_pnl < Decimal::ZERO)
        .map(|trade| -trade.realized_pnl)
        .sum();

    let profit_factor = if gross_loss.is_zero() {
        None
    } else {
        Some((gross_profit / gross_loss).round_dp(2))
    };

    (closed.len(), win_rate.round_dp(2), profit_factor)
}

fn max_drawdown(curve: &[EquityPoint]) -> Decimal {
    let mut peak = Decimal::ZERO;
    let mut worst = Decimal::ZERO;
    for point in curve {
        if point.equity > peak {
            peak = point.equity;
        }
        if peak > Decimal::ZERO {
            let drawdown = (peak - point.equity) / peak * Decimal::from(100);
            if drawdown > worst {
                worst = drawdown;
            }
        }
    }
    worst.round_dp(2)
}

fn benchmark_return(history: &HashMap<String, Vec<Candle>>, symbols: &[String]) -> Decimal {
    let mut total = Decimal::ZERO;
    let mut count = 0;
    for symbol in symbols {
        if let Some(candles) = history.get(symbol) {
            if let (Some(first), Some(last)) = (candles.first(), candles.last()) {
                if first.close > 0.0 {
                    let ratio = last.close / first.close;
                    if let Some(return_pct) = Decimal::from_f64_retain((ratio - 1.0) * 100.0) {
                        total += return_pct;
                        count += 1;
                    }
                }
            }
        }
    }
    if count == 0 {
        Decimal::ZERO
    } else {
        (total / Decimal::from(count)).round_dp(2)
    }
}

fn downsample(points: Vec<EquityPoint>, max_points: usize) -> Vec<EquityPoint> {
    if points.len() <= max_points || max_points < 2 {
        return points;
    }
    let stride = ((points.len() as f64) / (max_points as f64)).ceil() as usize;
    let mut sampled: Vec<EquityPoint> = points.iter().step_by(stride.max(1)).cloned().collect();
    if sampled.last().map(|point| point.timestamp) != points.last().map(|point| point.timestamp) {
        sampled.push(points.last().expect("points is not empty").clone());
    }
    sampled
}

fn downsample_prices(points: Vec<PricePoint>, max_points: usize) -> Vec<PricePoint> {
    if points.len() <= max_points || max_points < 2 {
        return points;
    }
    let stride = ((points.len() as f64) / (max_points as f64)).ceil() as usize;
    let mut sampled: Vec<PricePoint> = points.iter().step_by(stride.max(1)).cloned().collect();
    if sampled.last().map(|point| point.timestamp) != points.last().map(|point| point.timestamp) {
        if let Some(last) = points.last() {
            sampled.push(last.clone());
        }
    }
    sampled
}

const PRICE_COLORS: [&str; 4] = ["#a78bfa", "#cbd5e1", "#22d3ee", "#cc79a7"];

fn price_chart(prices: &HashMap<String, Vec<PricePoint>>, trades: &[Trade]) -> String {
    let mut series: Vec<(&str, &Vec<PricePoint>)> = prices
        .iter()
        .filter(|(_, points)| !points.is_empty())
        .map(|(symbol, points)| (symbol.as_str(), points))
        .collect();
    series.sort_by(|left, right| left.0.cmp(right.0));

    let (Some(t_min), Some(t_max)) = (
        series
            .iter()
            .filter_map(|(_, points)| points.first().map(|point| point.timestamp))
            .min(),
        series
            .iter()
            .filter_map(|(_, points)| points.last().map(|point| point.timestamp))
            .max(),
    ) else {
        return "<p class=\"empty\">No price history.</p>\n".to_string();
    };

    let width = 900.0_f64;
    let height = 260.0_f64;
    let pad_x = 64.0_f64;
    let pad_y = 24.0_f64;
    let t_span = (t_max - t_min).num_seconds().max(1) as f64;
    let x_at = |timestamp: DateTime<Utc>| -> f64 {
        pad_x + (timestamp - t_min).num_seconds().max(0) as f64 / t_span * (width - 2.0 * pad_x)
    };

    let raw_min = series
        .iter()
        .flat_map(|(_, points)| points.iter().map(|point| point.close))
        .fold(f64::INFINITY, f64::min);
    let raw_max = series
        .iter()
        .flat_map(|(_, points)| points.iter().map(|point| point.close))
        .fold(f64::NEG_INFINITY, f64::max);
    let padding = if raw_max > raw_min {
        (raw_max - raw_min) * 0.05
    } else {
        raw_max.abs().max(1.0) * 0.05
    };
    let min = raw_min - padding;
    let max = raw_max + padding;
    let span = max - min;
    let y_at = |value: f64| -> f64 {
        pad_y + (1.0 - (value - min) / span) * (height - 2.0 * pad_y)
    };

    let mut grid = String::new();
    for step in 0..=4 {
        let fraction = step as f64 / 4.0;
        let value = min + span * fraction;
        let y = y_at(value);
        grid.push_str(&format!(
            "<line x1=\"{pad_x}\" y1=\"{y:.2}\" x2=\"{x2:.2}\" y2=\"{y:.2}\" stroke=\"#24324a\" stroke-width=\"1\"></line><text x=\"{label_x:.2}\" y=\"{label_y:.2}\" fill=\"#94a3b8\" font-size=\"11\" text-anchor=\"end\">{value:.2}</text>",
            x2 = width - pad_x,
            label_x = pad_x - 8.0,
            label_y = y + 4.0,
        ));
    }

    let mut legend = String::new();
    for (index, (symbol, _)) in series.iter().enumerate() {
        let color = PRICE_COLORS[index % PRICE_COLORS.len()];
        let x = pad_x + index as f64 * 120.0;
        legend.push_str(&format!(
            "<rect x=\"{x:.2}\" y=\"2\" width=\"10\" height=\"10\" rx=\"2\" fill=\"{color}\"></rect><text x=\"{tx:.2}\" y=\"11\" fill=\"#94a3b8\" font-size=\"11\">{}</text>",
            report::esc(symbol),
            tx = x + 14.0,
        ));
    }
    let marker_x = width - pad_x - 92.0;
    legend.push_str(&format!(
        "<circle cx=\"{marker_x:.2}\" cy=\"7\" r=\"3.5\" fill=\"#0072b2\"></circle><text x=\"{buy_x:.2}\" y=\"11\" fill=\"#94a3b8\" font-size=\"11\">buy</text><rect x=\"{sell_cx:.2}\" y=\"3.5\" width=\"7\" height=\"7\" fill=\"#e69f00\"></rect><text x=\"{sell_x:.2}\" y=\"11\" fill=\"#94a3b8\" font-size=\"11\">sell</text>",
        buy_x = marker_x + 7.0,
        sell_cx = marker_x + 41.0,
        sell_x = marker_x + 51.0,
    ));

    let mut lines = String::new();
    for (index, (symbol, points)) in series.iter().enumerate() {
        let color = PRICE_COLORS[index % PRICE_COLORS.len()];
        let mut polyline = String::new();
        for (point_index, point) in points.iter().enumerate() {
            if point_index > 0 {
                polyline.push(' ');
            }
            polyline.push_str(&format!(
                "{:.2},{:.2}",
                x_at(point.timestamp),
                y_at(point.close)
            ));
        }
        lines.push_str(&format!(
            "<polyline points=\"{polyline}\" fill=\"none\" stroke=\"{color}\" stroke-width=\"1.8\" stroke-linejoin=\"round\" stroke-linecap=\"round\"><title>{}</title></polyline>\n",
            report::esc(symbol)
        ));
    }

    let mut markers = String::new();
    for trade in trades {
        if !prices.contains_key(&trade.symbol) {
            continue;
        }
        let Some(price) = trade.price.to_f64().filter(|value| value.is_finite()) else {
            continue;
        };
        let (color, label) = match trade.side {
            Side::Buy => ("#0072b2", "BUY"),
            Side::Sell => ("#e69f00", "SELL"),
        };
        let x = x_at(trade.timestamp);
        let y = y_at(price);
        let tip = format!(
            "<title>{} {} {} @ {:.2} ({})</title>",
            report::esc(&trade.symbol),
            label,
            trade.quantity,
            price,
            trade.timestamp.format("%Y-%m-%d"),
        );
        // Shape carries the side too, so it survives greyscale and any CVD.
        let shape = match trade.side {
            Side::Buy => format!(
                "<circle cx=\"{x:.2}\" cy=\"{y:.2}\" r=\"2.8\" fill=\"{color}\" fill-opacity=\"0.9\">{tip}</circle>\n"
            ),
            Side::Sell => format!(
                "<rect x=\"{rx:.2}\" y=\"{ry:.2}\" width=\"5.6\" height=\"5.6\" fill=\"{color}\" fill-opacity=\"0.9\">{tip}</rect>\n",
                rx = x - 2.8,
                ry = y - 2.8,
            ),
        };
        markers.push_str(&shape);
    }

    format!(
        "<svg class=\"chart\" viewBox=\"0 0 {width} {height}\" role=\"img\" aria-label=\"Price history with trades\">\n{grid}{legend}{lines}{markers}\n<text x=\"{pad_x}\" y=\"{label_y:.2}\" fill=\"#94a3b8\" font-size=\"11\">{first}</text>\n<text x=\"{last_x:.2}\" y=\"{label_y:.2}\" fill=\"#94a3b8\" font-size=\"11\" text-anchor=\"end\">{last}</text>\n</svg>\n",
        first = t_min.format("%Y-%m-%d"),
        last = t_max.format("%Y-%m-%d"),
        last_x = width - pad_x,
        label_y = height - 4.0,
    )
}


fn profit_factor_label(result: &BacktestResult) -> String {
    if result.closed_trades == 0 {
        "n/a".to_string()
    } else if let Some(value) = result.profit_factor {
        value.to_string()
    } else {
        "inf".to_string()
    }
}

fn print_summary(results: &[BacktestResult]) {
    let mut output = String::new();
    output.push_str(&format!(
        "{:<18} {:<8} {:<18} {:>10} {:>10} {:>8} {:>9} {:>8} {:>10}\n",
        "strategy",
        "interval",
        "symbols",
        "return",
        "max DD",
        "trades",
        "win rate",
        "PF",
        "benchmark"
    ));
    for result in results {
        output.push_str(&format!(
            "{:<18} {:<8} {:<18} {:>9}% {:>9}% {:>8} {:>8}% {:>8} {:>9}%\n",
            result.strategy,
            result.interval,
            result.symbols.join(","),
            result.return_pct.round_dp(2),
            result.max_drawdown_pct.round_dp(2),
            result.trades,
            result.win_rate_pct.round_dp(2),
            profit_factor_label(result),
            result.benchmark_return_pct.round_dp(2),
        ));
    }
    tracing::info!("backtest summary\n{}", output.trim_end());
}

fn write_json(path: &Path, results: &[BacktestResult]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let contents = serde_json::to_string_pretty(results)?;
    std::fs::write(path, contents)
        .with_context(|| format!("could not write {}", path.display()))?;
    tracing::info!(path = %path.display(), "backtest JSON written");
    Ok(())
}

fn write_html(path: &Path, config: &Config, results: &[BacktestResult]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let html = render_html(config, results);
    std::fs::write(path, html).with_context(|| format!("could not write {}", path.display()))?;
    tracing::info!(path = %path.display(), "backtest HTML written");
    Ok(())
}

fn render_html(config: &Config, results: &[BacktestResult]) -> String {
    let generated = Utc::now();
    let mut html = String::new();
    html.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    html.push_str("<title>bye-watch backtest report</title>\n<style>");
    html.push_str(report::CSS);
    html.push_str("</style>\n</head>\n<body>\n<div class=\"wrap\">\n");

    html.push_str("<header>\n<div>\n<div class=\"title-row\"><span class=\"badge\">BACKTEST</span><h1>bye-watch backtest</h1></div>\n");
    html.push_str(&format!(
        "<div class=\"sub\">Generated {} &middot; {} ledger &middot; Yahoo Finance history</div>\n</div>\n",
        report::timestamp(generated),
        report::esc(&config.market.quote_currency)
    ));
    html.push_str(&format!(
        "<div class=\"sub\">{} strategy run(s)</div>\n</header>\n",
        results.len()
    ));

    html.push_str("<section class=\"card\">\n<h2>Summary</h2>\n<table><thead><tr><th>Strategy</th><th>Symbols</th><th>Interval</th><th class=\"num\">Return</th><th class=\"num\">Max DD</th><th class=\"num\">Trades</th><th class=\"num\">Win rate</th><th class=\"num\">Profit factor</th><th class=\"num\">Benchmark</th></tr></thead><tbody>\n");
    for result in results {
        html.push_str(&format!(
            "<tr><td><strong>{}</strong></td><td>{}</td><td>{}</td><td class=\"num {}\">{}%</td><td class=\"num\">{}%</td><td class=\"num\">{}</td><td class=\"num\">{}%</td><td class=\"num\">{}</td><td class=\"num\">{}%</td></tr>\n",
            report::esc(&result.strategy),
            report::esc(&result.symbols.join(", ")),
            report::esc(&result.interval),
            report::pnl_class(result.return_pct),
            result.return_pct.round_dp(2),
            result.max_drawdown_pct.round_dp(2),
            result.trades,
            result.win_rate_pct.round_dp(2),
            report::esc(&profit_factor_label(result)),
            result.benchmark_return_pct.round_dp(2),
        ));
    }
    html.push_str("</tbody></table>\n</section>\n");

    for result in results {
        html.push_str(&format!(
            "<section class=\"card\">\n<h2>{} &middot; {} &middot; {}</h2>\n",
            report::esc(&result.strategy),
            report::esc(&result.interval),
            report::esc(&result.symbols.join(", "))
        ));
        html.push_str(&format!(
            "<div class=\"sub\" style=\"margin-bottom:14px\">{} to {} &middot; starting cash {}</div>\n",
            result
                .from
                .map(|date| date.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "earliest".to_string()),
            result
                .to
                .map(|date| date.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "latest".to_string()),
            report::money(result.starting_cash)
        ));

        html.push_str("<section class=\"kpis\">\n");
        html.push_str(&report::kpi(
            "Return",
            format!("{}%", result.return_pct.round_dp(2)),
            "net of costs",
            report::pnl_class(result.return_pct),
        ));
        html.push_str(&report::kpi(
            "Net PnL",
            report::money(result.net_pnl),
            "EUR",
            report::pnl_class(result.net_pnl),
        ));
        html.push_str(&report::kpi(
            "Max drawdown",
            format!("{}%", result.max_drawdown_pct),
            "peak to trough",
            "neg",
        ));
        html.push_str(&report::kpi(
            "Trades",
            result.trades.to_string(),
            &format!("{} closed", result.closed_trades),
            "",
        ));
        html.push_str(&report::kpi(
            "Win rate",
            format!("{}%", result.win_rate_pct),
            "closed trades",
            "",
        ));
        html.push_str(&report::kpi(
            "Profit factor",
            profit_factor_label(result),
            "gross profit / loss",
            "",
        ));
        html.push_str(&report::kpi(
            "Benchmark",
            format!("{}%", result.benchmark_return_pct),
            "equal-weight buy and hold",
            report::pnl_class(result.benchmark_return_pct),
        ));
        html.push_str("</section>\n");

        html.push_str("<h2>Equity curve</h2>\n");
        html.push_str(&report::equity_chart(&result.equity_curve));
        html.push_str("<h2 style=\"margin-top:20px\">Price &amp; trades</h2>\n");
        html.push_str(&price_chart(&result.price_series, &result.portfolio.trades));
        html.push_str("<h2 style=\"margin-top:20px\">Open positions</h2>\n");
        html.push_str(&report::positions_table(
            &result.portfolio,
            &result.final_prices,
            result.final_equity,
        ));
        html.push_str("<h2 style=\"margin-top:20px\">Trades</h2>\n");
        html.push_str(&report::trades_table(&result.portfolio));
        html.push_str("</section>\n");
    }

    html.push_str("<footer>Historical simulation only. Past performance is not indicative of future results. FX is held constant at the rate used when the backtest started.</footer>\n");
    html.push_str("</div>\n</body>\n</html>\n");
    html
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::build_registry;
    use engine::SourceConfig;

    fn candle(timestamp: i64, close: f64) -> Candle {
        Candle {
            timestamp,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.0,
        }
    }

    fn point(equity: i64) -> EquityPoint {
        EquityPoint {
            timestamp: Utc::now(),
            equity: Decimal::from(equity),
            cash: Decimal::ZERO,
            net_pnl: Decimal::ZERO,
        }
    }

    fn config() -> Config {
        toml::from_str(
            r#"
[paper]
starting_cash_eur = 1000.0

[engine]
max_history = 50

[backtest]
max_curve_points = 100

[[strategies]]
name = "ma_cross"
symbols = ["KO"]
interval = "1d"
params = { fast = 2, slow = 3 }
"#,
        )
        .expect("test config")
    }

    #[tokio::test]
    async fn simulates_a_crossing_strategy() {
        let config = config();
        let registry = build_registry();
        let source_config = SourceConfig {
            name: "ma_cross".to_string(),
            symbols: vec!["KO".to_string()],
            interval: "1d".to_string(),
            params: toml::from_str("fast = 2\nslow = 3").expect("params"),
        };
        let mut source = registry.build(&source_config).expect("source");

        let closes = [10.0, 10.0, 10.0, 10.0, 10.0, 20.0, 20.0, 20.0, 5.0, 5.0];
        let candles: Vec<Candle> = closes
            .iter()
            .enumerate()
            .map(|(index, close)| candle(index as i64, *close))
            .collect();
        let mut history = HashMap::new();
        history.insert("KO".to_string(), candles);

        let result = simulate(
            &config,
            source.as_mut(),
            "ma_cross",
            &["KO".to_string()],
            "1d",
            &history,
            Decimal::from(1_000),
            Decimal::ONE,
            None,
            None,
        )
        .await
        .expect("simulation");

        assert_eq!(result.trades, 2);
        assert_eq!(result.closed_trades, 1);
        assert!(!result.equity_curve.is_empty());
        assert!(result.max_drawdown_pct > Decimal::ZERO);
    }

    #[test]
    fn max_drawdown_tracks_peak_to_trough() {
        let curve = vec![point(100), point(120), point(90), point(110)];
        assert_eq!(max_drawdown(&curve), Decimal::from(25));
    }

    #[test]
    fn downsample_keeps_first_and_last() {
        let points: Vec<EquityPoint> = (0..10).map(point).collect();
        let sampled = downsample(points, 3);
        assert_eq!(sampled.first().unwrap().equity, Decimal::from(0));
        assert_eq!(sampled.last().unwrap().equity, Decimal::from(9));
        assert!(sampled.len() <= 4);
    }

    #[test]
    fn price_chart_marks_buys_and_sells() {
        let start = DateTime::<Utc>::from_timestamp(0, 0).expect("start");
        let end = DateTime::<Utc>::from_timestamp(86_400, 0).expect("end");
        let mut prices = HashMap::new();
        prices.insert(
            "KO".to_string(),
            vec![
                PricePoint {
                    timestamp: start,
                    close: 10.0,
                },
                PricePoint {
                    timestamp: end,
                    close: 12.0,
                },
            ],
        );
        let trade = |side: Side, price: i64| Trade {
            timestamp: end,
            symbol: "KO".to_string(),
            side,
            quantity: Decimal::ONE,
            price: Decimal::from(price),
            fees: Decimal::ZERO,
            slippage: Decimal::ZERO,
            realized_pnl: Decimal::ZERO,
            reason: "test".to_string(),
            confidence: 0.9,
            source: "rsi".to_string(),
        };

        let svg = price_chart(
            &prices,
            &[trade(Side::Buy, 12), trade(Side::Sell, 11)],
        );
        assert!(svg.contains("KO"));
        assert!(svg.contains("BUY"));
        assert!(svg.contains("SELL"));
        assert!(svg.contains("#0072b2"));
        assert!(svg.contains("#e69f00"));
        assert!(svg.contains("<rect"));
    }

    #[test]
    fn intraday_history_limits_are_known() {
        assert_eq!(max_intraday_days("1m"), Some(7));
        assert_eq!(max_intraday_days("5m"), Some(60));
        assert_eq!(max_intraday_days("1h"), Some(730));
        assert_eq!(max_intraday_days("1d"), None);
    }
}
