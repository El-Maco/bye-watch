use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;
use chrono::{DateTime, Utc};
use engine::{Portfolio, Side};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::config::Config;

const CSS: &str = r#"
:root {
  --bg: #f4f6fb;
  --card: #ffffff;
  --text: #0f172a;
  --muted: #64748b;
  --line: #e2e8f0;
  --accent: #2563eb;
  --green: #15803d;
  --red: #b91c1c;
  --amber: #b45309;
}
* { box-sizing: border-box; }
body {
  margin: 0;
  background: var(--bg);
  color: var(--text);
  font-family: ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
  line-height: 1.45;
}
.wrap { max-width: 1120px; margin: 0 auto; padding: 34px 20px 64px; }
header { display: flex; justify-content: space-between; align-items: flex-end; gap: 20px; margin-bottom: 24px; }
.title-row { display: flex; align-items: center; gap: 12px; }
h1 { font-size: 28px; margin: 0; letter-spacing: -0.02em; }
h2 { font-size: 16px; margin: 0 0 14px; letter-spacing: -0.01em; }
.sub { color: var(--muted); font-size: 13px; margin-top: 6px; }
.badge {
  display: inline-block; padding: 4px 10px; border-radius: 999px; font-size: 11px;
  font-weight: 700; letter-spacing: 0.08em; background: #dbeafe; color: #1d4ed8;
}
.kpis { display: grid; grid-template-columns: repeat(auto-fit, minmax(158px, 1fr)); gap: 12px; margin-bottom: 20px; }
.kpi { background: var(--card); border: 1px solid var(--line); border-radius: 12px; padding: 15px 16px; }
.kpi .label { color: var(--muted); font-size: 11px; text-transform: uppercase; letter-spacing: 0.07em; }
.kpi .value { font-size: 23px; font-weight: 700; margin-top: 6px; font-variant-numeric: tabular-nums; }
.kpi .sub { font-size: 12px; margin-top: 4px; }
.card { background: var(--card); border: 1px solid var(--line); border-radius: 14px; padding: 20px; margin-bottom: 20px; }
.grid2 { display: grid; grid-template-columns: 1fr 1fr; gap: 20px; }
table { width: 100%; border-collapse: collapse; font-size: 13px; }
th {
  text-align: left; color: var(--muted); font-weight: 700; font-size: 10px;
  text-transform: uppercase; letter-spacing: 0.06em; padding: 8px 10px;
  border-bottom: 1px solid var(--line); white-space: nowrap;
}
td { padding: 9px 10px; border-bottom: 1px solid var(--line); vertical-align: top; }
tr:last-child td { border-bottom: none; }
.num { text-align: right; font-variant-numeric: tabular-nums; white-space: nowrap; }
.pos { color: var(--green); }
.neg { color: var(--red); }
.muted { color: var(--muted); }
.side {
  display: inline-block; font-weight: 700; font-size: 10px; padding: 2px 8px;
  border-radius: 999px; letter-spacing: 0.05em;
}
.side.buy { background: #dcfce7; color: #166534; }
.side.sell { background: #fee2e2; color: #991b1b; }
.reason { max-width: 330px; color: #334155; }
.chart { width: 100%; height: auto; display: block; }
.empty { color: var(--muted); font-size: 13px; padding: 8px 0; }
.facts { display: grid; grid-template-columns: repeat(auto-fit, minmax(170px, 1fr)); gap: 12px; }
.fact { border: 1px solid var(--line); border-radius: 10px; padding: 12px; }
.fact-label { color: var(--muted); font-size: 10px; text-transform: uppercase; letter-spacing: 0.07em; }
.fact-value { font-size: 13px; font-weight: 600; margin-top: 5px; word-break: break-word; }
footer { color: var(--muted); font-size: 12px; margin-top: 24px; }
@media (max-width: 760px) {
  .grid2 { grid-template-columns: 1fr; }
  header { flex-direction: column; align-items: flex-start; }
}
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquityPoint {
    pub timestamp: DateTime<Utc>,
    pub equity: Decimal,
    pub cash: Decimal,
    pub net_pnl: Decimal,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReportHistory {
    #[serde(default)]
    pub points: Vec<EquityPoint>,
}

impl ReportHistory {
    fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(contents) => match serde_json::from_str(&contents) {
                Ok(history) => history,
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %error,
                        "could not parse report history; starting fresh"
                    );
                    Self::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %error,
                    "could not read report history; starting fresh"
                );
                Self::default()
            }
        }
    }

    fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let contents = serde_json::to_string_pretty(self)?;
        std::fs::write(path, contents)
            .with_context(|| format!("could not write {}", path.display()))?;
        Ok(())
    }

    fn push(&mut self, point: EquityPoint, max_points: usize) {
        self.points.push(point);
        if self.points.len() > max_points {
            let excess = self.points.len() - max_points;
            self.points.drain(0..excess);
        }
    }
}

pub fn write_report(
    config: &Config,
    portfolio: &Portfolio,
    prices: &HashMap<String, Decimal>,
) -> anyhow::Result<()> {
    if !config.report.enabled {
        return Ok(());
    }

    let history_path = Path::new(&config.report.history_path);
    let mut history = ReportHistory::load(history_path);
    history.push(
        EquityPoint {
            timestamp: Utc::now(),
            equity: portfolio.equity(prices),
            cash: portfolio.cash,
            net_pnl: portfolio.net_pnl(prices),
        },
        config.report.max_history_points,
    );
    history.save(history_path)?;

    let html = render_html(config, portfolio, prices, &history.points);
    let html_path = Path::new(&config.report.html_path);
    if let Some(parent) = html_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(html_path, html)
        .with_context(|| format!("could not write {}", html_path.display()))?;
    Ok(())
}

pub fn render_html(
    config: &Config,
    portfolio: &Portfolio,
    prices: &HashMap<String, Decimal>,
    history: &[EquityPoint],
) -> String {
    let generated = Utc::now();
    let equity = portfolio.equity(prices);
    let net_pnl = portfolio.net_pnl(prices);
    let gross_pnl = portfolio.gross_pnl(prices);
    let unrealized = portfolio.unrealized_pnl(prices);
    let return_pct = percentage(net_pnl, portfolio.starting_cash);
    let cash_pct = percentage(portfolio.cash, equity);
    let symbols = config_symbols(config);

    let mut html = String::new();
    html.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    html.push_str("<title>bye-watch paper trading report</title>\n<style>");
    html.push_str(CSS);
    html.push_str("</style>\n</head>\n<body>\n<div class=\"wrap\">\n");

    html.push_str("<header>\n<div>\n<div class=\"title-row\"><span class=\"badge\">PAPER</span><h1>bye-watch report</h1></div>\n");
    html.push_str(&format!(
        "<div class=\"sub\">Generated {} &middot; {} ledger &middot; starting cash {}</div>\n</div>\n",
        timestamp(generated),
        esc(&config.market.quote_currency),
        money(portfolio.starting_cash)
    ));
    html.push_str(&format!(
        "<div class=\"sub\">{} trades &middot; {} open positions</div>\n</header>\n",
        portfolio.trades.len(),
        portfolio.positions.len()
    ));

    html.push_str("<section class=\"kpis\">\n");
    html.push_str(&kpi(
        "Equity",
        money(equity),
        &format!("{} return", pct(return_pct)),
        pnl_class(net_pnl),
    ));
    html.push_str(&kpi(
        "Cash",
        money(portfolio.cash),
        &format!("{} of equity", pct(cash_pct)),
        "",
    ));
    html.push_str(&kpi(
        "Net PnL",
        money(net_pnl),
        "after costs",
        pnl_class(net_pnl),
    ));
    html.push_str(&kpi(
        "Gross PnL",
        money(gross_pnl),
        "costs added back",
        pnl_class(gross_pnl),
    ));
    html.push_str(&kpi(
        "Realized",
        money(portfolio.realized_pnl),
        "closed portions",
        pnl_class(portfolio.realized_pnl),
    ));
    html.push_str(&kpi(
        "Unrealized",
        money(unrealized),
        "open positions",
        pnl_class(unrealized),
    ));
    html.push_str(&kpi("Fees", money(portfolio.fees_paid), "commissions", ""));
    html.push_str(&kpi(
        "Slippage",
        money(portfolio.slippage_paid),
        "execution impact",
        "",
    ));
    html.push_str("</section>\n");

    html.push_str("<section class=\"card\">\n<h2>Equity curve</h2>\n");
    html.push_str(&equity_chart(history));
    html.push_str("</section>\n");

    html.push_str("<section class=\"card\">\n<h2>Open positions</h2>\n");
    html.push_str(&positions_table(portfolio, prices, equity));
    html.push_str("</section>\n");

    html.push_str(&format!(
        "<section class=\"card\">\n<h2>Trade history <span class=\"muted\">({})</span></h2>\n",
        portfolio.trades.len()
    ));
    html.push_str(&trades_table(portfolio));
    html.push_str("</section>\n");

    html.push_str("<div class=\"grid2\">\n");
    html.push_str("<section class=\"card\">\n<h2>Signal sources</h2>\n");
    html.push_str(&sources_table(portfolio));
    html.push_str("</section>\n");

    html.push_str("<section class=\"card\">\n<h2>Configuration</h2>\n<div class=\"facts\">\n");
    html.push_str(&fact("Universe", &esc(&symbols.join(", "))));
    html.push_str(&fact("Strategies", &strategy_summary(config)));
    html.push_str(&fact("Intervals", &interval_summary(config)));
    html.push_str(&fact(
        "Sizing",
        &format!(
            "{} (fraction {:.2}, max {:.2})",
            esc(&config.sizing.mode),
            config.sizing.fraction,
            config.sizing.max_fraction
        ),
    ));
    html.push_str(&fact("Fees", &fee_summary(config)));
    html.push_str(&fact(
        "LLM agent",
        &if config.agent.enabled {
            format!(
                "{} @ {}",
                esc(&config.agent.model),
                esc(&config.agent.base_url)
            )
        } else {
            "disabled".to_string()
        },
    ));
    html.push_str(&fact("Social", &esc(&config.social.provider)));
    html.push_str(&fact("Ledger", &esc(&config.market.quote_currency)));
    html.push_str(&fact("State file", &esc(&config.paper.state_path)));
    html.push_str("</div>\n</section>\n</div>\n");

    html.push_str("<footer>Paper trading only. No live orders were placed. Generated by bye-watch.</footer>\n");
    html.push_str("</div>\n</body>\n</html>\n");

    html
}

fn kpi(label: &str, value: String, sub: &str, class: &str) -> String {
    format!(
        "<div class=\"kpi\"><div class=\"label\">{}</div><div class=\"value {}\">{}</div><div class=\"sub\">{}</div></div>\n",
        esc(label),
        class,
        value,
        esc(sub)
    )
}

fn fact(label: &str, value: &str) -> String {
    format!(
        "<div class=\"fact\"><div class=\"fact-label\">{}</div><div class=\"fact-value\">{}</div></div>\n",
        esc(label),
        value
    )
}

fn positions_table(
    portfolio: &Portfolio,
    prices: &HashMap<String, Decimal>,
    equity: Decimal,
) -> String {
    if portfolio.positions.is_empty() {
        return "<p class=\"empty\">No open positions.</p>\n".to_string();
    }

    let mut positions: Vec<_> = portfolio.positions.values().collect();
    positions.sort_by(|left, right| left.symbol.cmp(&right.symbol));

    let mut html = String::from("<table><thead><tr><th>Symbol</th><th>Side</th><th class=\"num\">Quantity</th><th class=\"num\">Avg entry</th><th class=\"num\">Price</th><th class=\"num\">Market value</th><th class=\"num\">Unrealized</th><th class=\"num\">% equity</th></tr></thead><tbody>\n");
    for position in positions {
        let price = prices
            .get(&position.symbol)
            .copied()
            .unwrap_or(position.avg_entry_price);
        let market_value = position.quantity * price;
        let unrealized = position.unrealized_pnl(price);
        let weight = percentage(market_value, equity);
        let (side_class, side_label) = if position.is_short() {
            ("sell", "SHORT")
        } else {
            ("buy", "LONG")
        };
        html.push_str(&format!(
            "<tr><td><strong>{}</strong></td><td><span class=\"side {}\">{}</span></td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num {}\">{}</td><td class=\"num\">{}</td></tr>\n",
            esc(&position.symbol),
            side_class,
            side_label,
            position.quantity.abs(),
            money(position.avg_entry_price),
            money(price),
            money(market_value),
            pnl_class(unrealized),
            money(unrealized),
            pct(weight)
        ));
    }
    html.push_str("</tbody></table>\n");
    html
}

fn trades_table(portfolio: &Portfolio) -> String {
    if portfolio.trades.is_empty() {
        return "<p class=\"empty\">No trades yet.</p>\n".to_string();
    }

    let mut html = String::from("<table><thead><tr><th>Time</th><th>Source</th><th>Symbol</th><th>Side</th><th class=\"num\">Quantity</th><th class=\"num\">Price</th><th class=\"num\">Fees</th><th class=\"num\">Slippage</th><th class=\"num\">Conf.</th><th>Reason</th></tr></thead><tbody>\n");
    for trade in portfolio.trades.iter().rev().take(200) {
        let (side_class, side_label) = match trade.side {
            Side::Buy => ("buy", "BUY"),
            Side::Sell => ("sell", "SELL"),
        };
        html.push_str(&format!(
            "<tr><td class=\"muted\">{}</td><td>{}</td><td><strong>{}</strong></td><td><span class=\"side {}\">{}</span></td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{:.2}</td><td class=\"reason\">{}</td></tr>\n",
            trade.timestamp.format("%Y-%m-%d %H:%M:%S"),
            esc(&trade.source),
            esc(&trade.symbol),
            side_class,
            side_label,
            trade.quantity,
            money(trade.price),
            money(trade.fees),
            money(trade.slippage),
            trade.confidence,
            esc(&trade.reason)
        ));
    }
    html.push_str("</tbody></table>\n");
    html
}

#[derive(Default)]
struct SourceStats {
    trades: usize,
    buys: usize,
    sells: usize,
    fees: Decimal,
    slippage: Decimal,
}

fn sources_table(portfolio: &Portfolio) -> String {
    if portfolio.trades.is_empty() {
        return "<p class=\"empty\">No signals have traded yet.</p>\n".to_string();
    }

    let mut stats: HashMap<String, SourceStats> = HashMap::new();
    for trade in &portfolio.trades {
        let entry = stats.entry(trade.source.clone()).or_default();
        entry.trades += 1;
        match trade.side {
            Side::Buy => entry.buys += 1,
            Side::Sell => entry.sells += 1,
        }
        entry.fees += trade.fees;
        entry.slippage += trade.slippage;
    }

    let mut rows: Vec<_> = stats.into_iter().collect();
    rows.sort_by(|left, right| {
        right
            .1
            .trades
            .cmp(&left.1.trades)
            .then(left.0.cmp(&right.0))
    });

    let mut html = String::from("<table><thead><tr><th>Source</th><th class=\"num\">Trades</th><th class=\"num\">Buys</th><th class=\"num\">Sells</th><th class=\"num\">Fees</th><th class=\"num\">Slippage</th></tr></thead><tbody>\n");
    for (source, entry) in rows {
        html.push_str(&format!(
            "<tr><td><strong>{}</strong></td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>\n",
            esc(&source),
            entry.trades,
            entry.buys,
            entry.sells,
            money(entry.fees),
            money(entry.slippage)
        ));
    }
    html.push_str("</tbody></table>\n");
    html
}

fn equity_chart(history: &[EquityPoint]) -> String {
    if history.len() < 2 {
        return "<p class=\"empty\">Not enough history yet. The curve fills in as the bot runs.</p>\n"
            .to_string();
    }

    let stride = (history.len() / 240).max(1);
    let mut points: Vec<&EquityPoint> = history.iter().step_by(stride).collect();
    if points.last().map(|point| point.timestamp) != history.last().map(|point| point.timestamp) {
        points.push(history.last().expect("history is not empty"));
    }

    let width = 900.0_f64;
    let height = 220.0_f64;
    let pad_x = 46.0_f64;
    let pad_y = 18.0_f64;
    let raw_min = points
        .iter()
        .map(|point| point.equity)
        .min()
        .unwrap_or(Decimal::ZERO);
    let raw_max = points
        .iter()
        .map(|point| point.equity)
        .max()
        .unwrap_or(Decimal::ZERO);
    let raw_span = (raw_max - raw_min).max(Decimal::new(1, 2));
    let padding = raw_span * Decimal::new(10, 2);
    let min = raw_min - padding;
    let max = raw_max + padding;
    let span = max - min;
    let count = points.len();

    let x_at = |index: usize| -> f64 {
        pad_x + (index as f64) * (width - 2.0 * pad_x) / ((count - 1) as f64)
    };
    let y_at = |value: Decimal| -> f64 {
        let ratio = ((value - min) / span).to_f64().unwrap_or(0.0);
        pad_y + (1.0 - ratio) * (height - 2.0 * pad_y)
    };

    let mut grid = String::new();
    for step in 0..=4 {
        let fraction = Decimal::from(step) / Decimal::from(4);
        let value = min + span * fraction;
        let y = pad_y + (1.0 - fraction.to_f64().unwrap_or(0.0)) * (height - 2.0 * pad_y);
        grid.push_str(&format!(
            "<line x1=\"{pad_x}\" y1=\"{y:.2}\" x2=\"{x2:.2}\" y2=\"{y:.2}\" stroke=\"#e2e8f0\" stroke-width=\"1\"></line><text x=\"{label_x:.2}\" y=\"{label_y:.2}\" fill=\"#94a3b8\" font-size=\"11\" text-anchor=\"end\">{value}</text>",
            x2 = width - pad_x,
            label_x = pad_x - 8.0,
            label_y = y + 4.0,
            value = money(value),
        ));
    }

    let mut polyline = String::new();
    for (index, point) in points.iter().enumerate() {
        if index > 0 {
            polyline.push(' ');
        }
        polyline.push_str(&format!("{:.2},{:.2}", x_at(index), y_at(point.equity)));
    }

    let mut area = format!("M {:.2},{:.2}", x_at(0), height - pad_y);
    for (index, point) in points.iter().enumerate() {
        area.push_str(&format!(" L {:.2},{:.2}", x_at(index), y_at(point.equity)));
    }
    area.push_str(&format!(
        " L {:.2},{:.2} Z",
        x_at(count - 1),
        height - pad_y
    ));

    let first = &points[0];
    let last = &points[count - 1];

    format!(
        "<svg class=\"chart\" viewBox=\"0 0 {width} {height}\" role=\"img\" aria-label=\"Equity curve\">\n{grid}\n<path d=\"{area}\" fill=\"rgba(37,99,235,0.12)\" stroke=\"none\"></path>\n<polyline points=\"{polyline}\" fill=\"none\" stroke=\"#2563eb\" stroke-width=\"2.5\" stroke-linejoin=\"round\" stroke-linecap=\"round\"></polyline>\n<circle cx=\"{last_x:.2}\" cy=\"{last_y:.2}\" r=\"3.5\" fill=\"#2563eb\"></circle>\n<text x=\"{pad_x}\" y=\"{label_y:.2}\" fill=\"#94a3b8\" font-size=\"11\">{first_label}</text>\n<text x=\"{last_label_x:.2}\" y=\"{label_y:.2}\" fill=\"#94a3b8\" font-size=\"11\" text-anchor=\"end\">{last_label}</text>\n</svg>\n",
        grid = grid,
        area = area,
        polyline = polyline,
        last_x = x_at(count - 1),
        last_y = y_at(last.equity),
        label_y = height - 4.0,
        first_label = first.timestamp.format("%Y-%m-%d %H:%M"),
        last_label_x = width - pad_x,
        last_label = last.timestamp.format("%Y-%m-%d %H:%M"),
    )
}

fn config_symbols(config: &Config) -> Vec<String> {
    let mut symbols: Vec<String> = config
        .strategies
        .iter()
        .flat_map(|strategy| strategy.symbols.iter().cloned())
        .collect();
    if config.agent.enabled {
        symbols.extend(config.agent_symbols());
    }
    symbols.sort();
    symbols.dedup();
    symbols
}

fn strategy_summary(config: &Config) -> String {
    if config.strategies.is_empty() {
        return "none".to_string();
    }
    config
        .strategies
        .iter()
        .map(|strategy| {
            format!(
                "{} ({}, {})",
                esc(&strategy.name),
                esc(&strategy.interval),
                esc(&strategy.symbols.join("/"))
            )
        })
        .collect::<Vec<_>>()
        .join(" &middot; ")
}

fn interval_summary(config: &Config) -> String {
    let mut intervals: Vec<String> = config
        .strategies
        .iter()
        .map(|strategy| strategy.interval.clone())
        .collect();
    if config.agent.enabled {
        intervals.push(config.agent.interval.clone());
    }
    intervals.sort();
    intervals.dedup();
    if intervals.is_empty() {
        "none".to_string()
    } else {
        esc(&intervals.join(", "))
    }
}

fn fee_summary(config: &Config) -> String {
    if !config.fees.enabled {
        return "disabled".to_string();
    }
    format!(
        "flat {:.2} + {:.3}% + {:.1} bps",
        config.fees.commission_flat,
        config.fees.commission_pct * 100.0,
        config.fees.slippage_bps
    )
}

fn percentage(value: Decimal, base: Decimal) -> Decimal {
    if base.is_zero() {
        Decimal::ZERO
    } else {
        (value / base * Decimal::from(100)).round_dp(2)
    }
}

fn pnl_class(value: Decimal) -> &'static str {
    if value > Decimal::ZERO {
        "pos"
    } else if value < Decimal::ZERO {
        "neg"
    } else {
        "muted"
    }
}

fn money(value: Decimal) -> String {
    value.round_dp(2).to_string()
}

fn pct(value: Decimal) -> String {
    format!("{}%", value)
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

fn esc(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{Fill, Portfolio};

    fn config() -> Config {
        toml::from_str(
            r#"
[report]
html_path = "unused.html"
history_path = "unused.json"

[[strategies]]
name = "rsi"
symbols = ["KO", "PEP"]
interval = "1d"
params = { period = 14 }
"#,
        )
        .expect("test config")
    }

    fn portfolio() -> Portfolio {
        let mut portfolio = Portfolio::new(Decimal::from(1_000));
        portfolio.apply_fill(&Fill {
            timestamp: Utc::now(),
            symbol: "KO".to_string(),
            side: Side::Buy,
            quantity: Decimal::from(10),
            price: Decimal::from(10),
            fees: Decimal::new(5, 1),
            slippage: Decimal::new(2, 1),
            reason: "test <buy> & hold".to_string(),
            confidence: 0.8,
            source: "rsi".to_string(),
        });
        portfolio
    }

    #[test]
    fn renders_dashboard_sections_and_escapes_text() {
        let config = config();
        let portfolio = portfolio();
        let mut prices = HashMap::new();
        prices.insert("KO".to_string(), Decimal::from(12));
        let history = vec![
            EquityPoint {
                timestamp: Utc::now(),
                equity: Decimal::from(1_000),
                cash: Decimal::from(1_000),
                net_pnl: Decimal::ZERO,
            },
            EquityPoint {
                timestamp: Utc::now(),
                equity: Decimal::from(1_020),
                cash: Decimal::from(880),
                net_pnl: Decimal::from(20),
            },
        ];

        let html = render_html(&config, &portfolio, &prices, &history);
        assert!(html.contains("bye-watch report"));
        assert!(html.contains("Open positions"));
        assert!(html.contains("Trade history"));
        assert!(html.contains("Signal sources"));
        assert!(html.contains("test &lt;buy&gt; &amp; hold"));
        assert!(html.contains("<svg"));
        assert!(html.contains("KO"));
    }

    #[test]
    fn escapes_html() {
        assert_eq!(
            esc("<b>&\"x\"</b>"),
            "&lt;b&gt;&amp;&quot;x&quot;&lt;/b&gt;"
        );
    }

    #[test]
    fn history_is_capped() {
        let mut history = ReportHistory::default();
        for index in 0..10 {
            history.push(
                EquityPoint {
                    timestamp: Utc::now(),
                    equity: Decimal::from(index),
                    cash: Decimal::ZERO,
                    net_pnl: Decimal::ZERO,
                },
                3,
            );
        }
        assert_eq!(history.points.len(), 3);
        assert_eq!(history.points.last().unwrap().equity, Decimal::from(9));
    }
}
