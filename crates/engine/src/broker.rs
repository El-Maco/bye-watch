use std::path::Path;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::error::{EngineError, Result};
use crate::fees::FeeModel;
use crate::portfolio::{Fill, Portfolio};
use crate::types::Side;

#[derive(Debug, Clone)]
pub struct OrderRequest {
    pub symbol: String,
    pub side: Side,
    pub quantity: Decimal,
    pub reference_price: Decimal,
    pub reason: String,
    pub confidence: f64,
    pub source: String,
    /// Optional clock override, used by backtests to stamp simulated fills.
    pub timestamp: Option<DateTime<Utc>>,
}

#[async_trait]
pub trait Broker: Send + Sync {
    async fn place_order(&mut self, order: &OrderRequest) -> Result<Fill>;
    fn portfolio(&self) -> &Portfolio;
    fn persist(&self, path: &Path) -> Result<()>;
}

pub struct PaperBroker {
    portfolio: Portfolio,
    fees: FeeModel,
    allow_short: bool,
}

impl PaperBroker {
    pub fn new(portfolio: Portfolio, fees: FeeModel, allow_short: bool) -> Self {
        Self {
            portfolio,
            fees,
            allow_short,
        }
    }

    pub fn load_or_new(
        path: &Path,
        starting_cash: Decimal,
        fees: FeeModel,
        allow_short: bool,
    ) -> Result<Self> {
        if path.exists() {
            let data = std::fs::read_to_string(path)?;
            let portfolio: Portfolio = serde_json::from_str(&data)?;
            tracing::info!(
                path = %path.display(),
                cash = %portfolio.cash,
                trades = portfolio.trades.len(),
                "loaded paper portfolio"
            );
            Ok(Self::new(portfolio, fees, allow_short))
        } else {
            Ok(Self::new(Portfolio::new(starting_cash), fees, allow_short))
        }
    }
}

#[async_trait]
impl Broker for PaperBroker {
    async fn place_order(&mut self, order: &OrderRequest) -> Result<Fill> {
        if order.quantity <= Decimal::ZERO {
            return Err(EngineError::Broker(
                "order quantity must be positive".to_string(),
            ));
        }
        if order.reference_price <= Decimal::ZERO {
            return Err(EngineError::Broker(
                "reference price must be positive".to_string(),
            ));
        }

        let (price, fees) = self
            .fees
            .apply(order.side, order.reference_price, order.quantity);
        let notional = order.quantity * price;
        let slippage = (price - order.reference_price).abs() * order.quantity;

        match order.side {
            Side::Buy => {
                if self.portfolio.cash < notional + fees {
                    return Err(EngineError::Broker(format!(
                        "insufficient cash for {} {}: need {} {}, have {}",
                        order.symbol,
                        order.quantity,
                        notional + fees,
                        "EUR",
                        self.portfolio.cash
                    )));
                }
            }
            Side::Sell => {
                let current = self.portfolio.position_qty(&order.symbol);
                if current < order.quantity && !self.allow_short {
                    return Err(EngineError::Broker(format!(
                        "shorting disabled and position in {} is only {}",
                        order.symbol, current
                    )));
                }
            }
        }

        let fill = Fill {
            timestamp: order.timestamp.unwrap_or_else(Utc::now),
            symbol: order.symbol.clone(),
            side: order.side,
            quantity: order.quantity,
            price,
            fees,
            slippage,
            reason: order.reason.clone(),
            confidence: order.confidence,
            source: order.source.clone(),
        };

        self.portfolio.apply_fill(&fill);
        Ok(fill)
    }

    fn portfolio(&self) -> &Portfolio {
        &self.portfolio
    }

    fn persist(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let data = serde_json::to_string_pretty(&self.portfolio)?;
        std::fs::write(path, data)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portfolio::Portfolio;

    fn order(side: Side, quantity: i64, price: i64) -> OrderRequest {
        OrderRequest {
            symbol: "KO".to_string(),
            side,
            quantity: Decimal::from(quantity),
            reference_price: Decimal::from(price),
            reason: "test".to_string(),
            confidence: 0.5,
            source: "test".to_string(),
            timestamp: None,
        }
    }

    fn fee_free() -> FeeModel {
        FeeModel {
            enabled: false,
            commission_flat: Decimal::ZERO,
            commission_pct: Decimal::ZERO,
            slippage_bps: Decimal::ZERO,
        }
    }

    #[tokio::test]
    async fn paper_broker_buys_and_sells() {
        let mut broker = PaperBroker::new(Portfolio::new(Decimal::from(1_000)), fee_free(), true);
        let fill = broker.place_order(&order(Side::Buy, 10, 10)).await.unwrap();
        assert_eq!(fill.quantity, Decimal::from(10));
        assert_eq!(broker.portfolio().position_qty("KO"), Decimal::from(10));

        broker
            .place_order(&order(Side::Sell, 10, 12))
            .await
            .unwrap();
        assert_eq!(broker.portfolio().position_qty("KO"), Decimal::ZERO);
        assert_eq!(broker.portfolio().realized_pnl, Decimal::from(20));
    }

    #[tokio::test]
    async fn rejects_order_without_cash() {
        let mut broker = PaperBroker::new(Portfolio::new(Decimal::from(50)), fee_free(), true);
        let result = broker.place_order(&order(Side::Buy, 10, 10)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn shorting_can_be_disabled() {
        let mut broker = PaperBroker::new(Portfolio::new(Decimal::from(1_000)), fee_free(), false);
        let result = broker.place_order(&order(Side::Sell, 1, 10)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn shorting_can_be_enabled() {
        let mut broker = PaperBroker::new(Portfolio::new(Decimal::from(1_000)), fee_free(), true);
        broker.place_order(&order(Side::Sell, 5, 10)).await.unwrap();
        assert_eq!(broker.portfolio().position_qty("KO"), Decimal::from(-5));
    }
}
