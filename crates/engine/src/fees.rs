use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::types::Side;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeConfig {
    /// When false, all trading costs are ignored in fills and PnL.
    pub enabled: bool,
    pub commission_flat: Decimal,
    /// Fraction of notional, e.g. 0.001 for 0.1%.
    pub commission_pct: Decimal,
    /// Slippage in basis points, e.g. 5 for 0.05%.
    pub slippage_bps: Decimal,
}

impl Default for FeeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            commission_flat: Decimal::ZERO,
            commission_pct: Decimal::ZERO,
            slippage_bps: Decimal::new(5, 0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeeModel {
    pub enabled: bool,
    pub commission_flat: Decimal,
    pub commission_pct: Decimal,
    pub slippage_bps: Decimal,
}

impl Default for FeeModel {
    fn default() -> Self {
        Self::from(FeeConfig::default())
    }
}

impl From<FeeConfig> for FeeModel {
    fn from(config: FeeConfig) -> Self {
        Self {
            enabled: config.enabled,
            commission_flat: config.commission_flat,
            commission_pct: config.commission_pct,
            slippage_bps: config.slippage_bps,
        }
    }
}

impl FeeModel {
    /// Returns the execution price after slippage and the commission in the
    /// quote currency.
    pub fn apply(
        &self,
        side: Side,
        reference_price: Decimal,
        quantity: Decimal,
    ) -> (Decimal, Decimal) {
        if !self.enabled {
            return (reference_price, Decimal::ZERO);
        }

        let slippage = self.slippage_bps / Decimal::from(10_000u32);
        let fill_price = match side {
            Side::Buy => reference_price * (Decimal::ONE + slippage),
            Side::Sell => reference_price * (Decimal::ONE - slippage),
        };
        let notional = reference_price * quantity;
        let commission = self.commission_flat + notional * self.commission_pct;

        (fill_price.round_dp(6), commission.round_dp(6))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_fees_leave_price_untouched() {
        let model = FeeModel {
            enabled: false,
            commission_flat: Decimal::from(1),
            commission_pct: Decimal::new(1, 2),
            slippage_bps: Decimal::from(50),
        };
        let (price, fee) = model.apply(Side::Buy, Decimal::from(100), Decimal::from(2));
        assert_eq!(price, Decimal::from(100));
        assert_eq!(fee, Decimal::ZERO);
    }

    #[test]
    fn slippage_and_commission_are_applied() {
        let model = FeeModel {
            enabled: true,
            commission_flat: Decimal::from(1),
            commission_pct: Decimal::new(1, 2),
            slippage_bps: Decimal::from(10),
        };
        let (price, fee) = model.apply(Side::Buy, Decimal::from(100), Decimal::from(1));
        assert_eq!(price, Decimal::new(1001, 1));
        assert_eq!(fee, Decimal::from(2));
    }
}
