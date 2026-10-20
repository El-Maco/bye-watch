use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::error::Result;
use crate::types::Side;

fn money(value: Decimal) -> String {
    format!("{}", value.round_dp(2))
}

#[derive(Debug, Clone)]
pub struct ActionReport {
    pub timestamp: DateTime<Utc>,
    pub symbol: String,
    pub side: Side,
    pub quantity: Decimal,
    pub price: Decimal,
    pub fees: Decimal,
    pub confidence: f64,
    pub source: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct PositionSummary {
    pub symbol: String,
    pub quantity: Decimal,
    pub avg_entry_price: Decimal,
    pub market_value: Decimal,
    pub unrealized_pnl: Decimal,
}

#[derive(Debug, Clone)]
pub struct PortfolioSummary {
    pub cash: Decimal,
    pub equity: Decimal,
    pub net_pnl: Decimal,
    pub gross_pnl: Decimal,
    pub realized_pnl: Decimal,
    pub unrealized_pnl: Decimal,
    pub fees_paid: Decimal,
    pub slippage_paid: Decimal,
    pub open_positions: Vec<PositionSummary>,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub timestamp: DateTime<Utc>,
    pub actions: Vec<ActionReport>,
    pub skipped: Vec<String>,
    pub portfolio: PortfolioSummary,
}

pub trait Notifier: Send + Sync {
    fn notify(&self, report: &Report) -> Result<()>;
}

pub struct ConsoleNotifier;

impl Notifier for ConsoleNotifier {
    fn notify(&self, report: &Report) -> Result<()> {
        let mut out = String::new();
        out.push_str(&format!(
            "=== bye-watch report {} ===\n",
            report.timestamp.format("%Y-%m-%d %H:%M:%S UTC")
        ));

        if report.actions.is_empty() {
            out.push_str("No paper trades this tick.\n");
        } else {
            for action in &report.actions {
                out.push_str(&format!(
                    "{} {} {} @ {} EUR | fee {} | conf {:.2} | {} | {}\n",
                    action.side.as_str().to_uppercase(),
                    action.quantity,
                    action.symbol,
                    money(action.price),
                    money(action.fees),
                    action.confidence,
                    action.source,
                    action.reason
                ));
            }
        }

        for skipped in &report.skipped {
            out.push_str(&format!("skipped: {skipped}\n"));
        }

        let portfolio = &report.portfolio;
        out.push_str(&format!(
            "cash {} EUR | equity {} EUR | net PnL {} | gross PnL {} | fees {} | slippage {}\n",
            money(portfolio.cash),
            money(portfolio.equity),
            money(portfolio.net_pnl),
            money(portfolio.gross_pnl),
            money(portfolio.fees_paid),
            money(portfolio.slippage_paid)
        ));

        if portfolio.open_positions.is_empty() {
            out.push_str("positions: none\n");
        } else {
            for position in &portfolio.open_positions {
                out.push_str(&format!(
                    "position {} {} @ {} EUR | value {} | unrealized {} | side {}\n",
                    position.quantity,
                    position.symbol,
                    money(position.avg_entry_price),
                    money(position.market_value),
                    money(position.unrealized_pnl),
                    if position.quantity.is_sign_negative() {
                        "short"
                    } else {
                        "long"
                    }
                ));
            }
        }

        tracing::info!("{}", out.trim_end());
        Ok(())
    }
}
