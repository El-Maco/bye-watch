use std::collections::HashMap;

use rust_decimal::Decimal;

use crate::error::{EngineError, Result};
use crate::types::{PortfolioView, SignalIntent};

pub trait PositionSizer: Send + Sync {
    fn size(
        &self,
        intent: &SignalIntent,
        portfolio: &PortfolioView,
        prices: &HashMap<String, Decimal>,
    ) -> Result<Decimal>;

    fn describe(&self) -> String;
}

fn hard_max_fraction() -> Decimal {
    // Strictly below 1.0 so the bot can never go all-in.
    Decimal::new(95, 2)
}

fn effective_fraction(configured: Decimal, max_fraction: Decimal) -> Decimal {
    let cap = hard_max_fraction();
    let mut fraction = configured.max(Decimal::ZERO);
    fraction = fraction.min(max_fraction.max(Decimal::ZERO));
    fraction.min(cap)
}

fn price_for(symbol: &str, prices: &HashMap<String, Decimal>) -> Result<Decimal> {
    let price = prices
        .get(symbol)
        .copied()
        .ok_or_else(|| EngineError::Config(format!("no price available for {symbol}")))?;
    if price <= Decimal::ZERO {
        return Err(EngineError::Config(format!(
            "price for {symbol} must be positive"
        )));
    }
    Ok(price)
}

#[derive(Debug, Clone)]
pub struct FixedSizer {
    pub quantity: Decimal,
    pub max_fraction: Decimal,
    pub use_quantity_hint: bool,
}

impl PositionSizer for FixedSizer {
    fn size(
        &self,
        intent: &SignalIntent,
        portfolio: &PortfolioView,
        prices: &HashMap<String, Decimal>,
    ) -> Result<Decimal> {
        if intent.close_position {
            return Ok(portfolio.position_qty(&intent.symbol).abs());
        }

        let price = price_for(&intent.symbol, prices)?;
        let notional_cap = portfolio.equity * effective_fraction(Decimal::ONE, self.max_fraction);
        let quantity_cap = notional_cap / price;
        let mut quantity = self.quantity.max(Decimal::ZERO).min(quantity_cap);

        if self.use_quantity_hint {
            if let Some(hint) = intent.quantity_hint {
                if hint > Decimal::ZERO {
                    quantity = quantity.min(hint);
                }
            }
        }

        Ok(quantity.round_dp(6))
    }

    fn describe(&self) -> String {
        format!(
            "fixed(quantity={}, max_fraction={})",
            self.quantity, self.max_fraction
        )
    }
}

#[derive(Debug, Clone)]
pub struct EquityFractionSizer {
    pub fraction: Decimal,
    pub max_fraction: Decimal,
    pub use_quantity_hint: bool,
}

impl PositionSizer for EquityFractionSizer {
    fn size(
        &self,
        intent: &SignalIntent,
        portfolio: &PortfolioView,
        prices: &HashMap<String, Decimal>,
    ) -> Result<Decimal> {
        if intent.close_position {
            return Ok(portfolio.position_qty(&intent.symbol).abs());
        }

        let price = price_for(&intent.symbol, prices)?;
        let fraction = effective_fraction(self.fraction, self.max_fraction);
        let notional = portfolio.equity * fraction;
        let mut quantity = (notional / price).max(Decimal::ZERO);

        if self.use_quantity_hint {
            if let Some(hint) = intent.quantity_hint {
                if hint > Decimal::ZERO {
                    quantity = quantity.min(hint);
                }
            }
        }

        Ok(quantity.round_dp(6))
    }

    fn describe(&self) -> String {
        format!(
            "equity_fraction(fraction={}, max_fraction={})",
            self.fraction, self.max_fraction
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PositionView, Side};

    fn view(equity: i64, quantity: Option<i64>) -> PortfolioView {
        let mut positions = HashMap::new();
        if let Some(quantity) = quantity {
            positions.insert(
                "KO".to_string(),
                PositionView {
                    quantity: Decimal::from(quantity),
                    avg_entry_price: Decimal::from(10),
                },
            );
        }
        PortfolioView {
            cash: Decimal::ZERO,
            equity: Decimal::from(equity),
            positions,
        }
    }

    fn prices(price: i64) -> HashMap<String, Decimal> {
        let mut prices = HashMap::new();
        prices.insert("KO".to_string(), Decimal::from(price));
        prices
    }

    #[test]
    fn fraction_is_capped_and_never_all_in() {
        let sizer = EquityFractionSizer {
            fraction: Decimal::ONE,
            max_fraction: Decimal::ONE,
            use_quantity_hint: false,
        };
        let intent = SignalIntent::new("KO", Side::Buy, 0.7, "entry", "test");
        let quantity = sizer
            .size(&intent, &view(1_000, None), &prices(10))
            .unwrap();
        assert_eq!(quantity, Decimal::from(95));
    }

    #[test]
    fn close_position_uses_current_position_size() {
        let sizer = EquityFractionSizer {
            fraction: Decimal::new(25, 2),
            max_fraction: Decimal::new(25, 2),
            use_quantity_hint: false,
        };
        let intent = SignalIntent::closing("KO", Side::Sell, 0.9, "exit", "test");
        let quantity = sizer
            .size(&intent, &view(1_000, Some(7)), &prices(10))
            .unwrap();
        assert_eq!(quantity, Decimal::from(7));
    }

    #[test]
    fn fixed_size_is_capped_by_max_fraction() {
        let sizer = FixedSizer {
            quantity: Decimal::from(1_000),
            max_fraction: Decimal::new(25, 2),
            use_quantity_hint: false,
        };
        let intent = SignalIntent::new("KO", Side::Buy, 0.7, "entry", "test");
        let quantity = sizer
            .size(&intent, &view(1_000, None), &prices(10))
            .unwrap();
        assert_eq!(quantity, Decimal::from(25));
    }
}
