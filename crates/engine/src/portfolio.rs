use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::types::Side;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub symbol: String,
    /// Signed: positive is long, negative is short.
    pub quantity: Decimal,
    pub avg_entry_price: Decimal,
}

impl Position {
    pub fn is_long(&self) -> bool {
        self.quantity > Decimal::ZERO
    }

    pub fn is_short(&self) -> bool {
        self.quantity < Decimal::ZERO
    }

    pub fn market_value(&self, price: Decimal) -> Decimal {
        self.quantity * price
    }

    pub fn unrealized_pnl(&self, price: Decimal) -> Decimal {
        if self.quantity >= Decimal::ZERO {
            (price - self.avg_entry_price) * self.quantity
        } else {
            (self.avg_entry_price - price) * self.quantity.abs()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub timestamp: DateTime<Utc>,
    pub symbol: String,
    pub side: Side,
    pub quantity: Decimal,
    pub price: Decimal,
    pub fees: Decimal,
    #[serde(default)]
    pub slippage: Decimal,
    pub reason: String,
    pub confidence: f64,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub timestamp: DateTime<Utc>,
    pub symbol: String,
    pub side: Side,
    pub quantity: Decimal,
    pub price: Decimal,
    pub fees: Decimal,
    #[serde(default)]
    pub slippage: Decimal,
    #[serde(default)]
    pub realized_pnl: Decimal,
    pub reason: String,
    pub confidence: f64,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Portfolio {
    pub starting_cash: Decimal,
    pub cash: Decimal,
    pub positions: HashMap<String, Position>,
    pub trades: Vec<Trade>,
    /// Realized PnL before fees, from closed portions of positions.
    pub realized_pnl: Decimal,
    pub fees_paid: Decimal,
    #[serde(default)]
    pub slippage_paid: Decimal,
}

impl Portfolio {
    pub fn new(starting_cash: Decimal) -> Self {
        Self {
            starting_cash,
            cash: starting_cash,
            positions: HashMap::new(),
            trades: Vec::new(),
            realized_pnl: Decimal::ZERO,
            fees_paid: Decimal::ZERO,
            slippage_paid: Decimal::ZERO,
        }
    }

    pub fn position(&self, symbol: &str) -> Option<&Position> {
        self.positions.get(symbol)
    }

    pub fn position_qty(&self, symbol: &str) -> Decimal {
        self.positions
            .get(symbol)
            .map(|p| p.quantity)
            .unwrap_or(Decimal::ZERO)
    }

    pub fn equity(&self, prices: &HashMap<String, Decimal>) -> Decimal {
        let mut equity = self.cash;
        for (symbol, position) in &self.positions {
            let price = prices
                .get(symbol)
                .copied()
                .unwrap_or(position.avg_entry_price);
            equity += position.quantity * price;
        }
        equity
    }

    pub fn unrealized_pnl(&self, prices: &HashMap<String, Decimal>) -> Decimal {
        self.positions
            .iter()
            .map(|(symbol, position)| {
                let price = prices
                    .get(symbol)
                    .copied()
                    .unwrap_or(position.avg_entry_price);
                position.unrealized_pnl(price)
            })
            .sum()
    }

    /// Equity if no trading costs had ever been charged.
    pub fn gross_equity(&self, prices: &HashMap<String, Decimal>) -> Decimal {
        self.equity(prices) + self.fees_paid + self.slippage_paid
    }

    pub fn net_pnl(&self, prices: &HashMap<String, Decimal>) -> Decimal {
        self.equity(prices) - self.starting_cash
    }

    pub fn gross_pnl(&self, prices: &HashMap<String, Decimal>) -> Decimal {
        self.gross_equity(prices) - self.starting_cash
    }

    pub fn apply_fill(&mut self, fill: &Fill) -> Decimal {
        let notional = fill.quantity * fill.price;
        match fill.side {
            Side::Buy => self.cash -= notional + fill.fees,
            Side::Sell => self.cash += notional - fill.fees,
        }
        self.fees_paid += fill.fees;
        self.slippage_paid += fill.slippage;

        let realized = self.apply_to_position(fill);
        self.realized_pnl += realized;
        self.trades.push(Trade {
            timestamp: fill.timestamp,
            symbol: fill.symbol.clone(),
            side: fill.side,
            quantity: fill.quantity,
            price: fill.price,
            fees: fill.fees,
            slippage: fill.slippage,
            realized_pnl: realized,
            reason: fill.reason.clone(),
            confidence: fill.confidence,
            source: fill.source.clone(),
        });
        realized
    }

    fn apply_to_position(&mut self, fill: &Fill) -> Decimal {
        let signed = match fill.side {
            Side::Buy => fill.quantity,
            Side::Sell => -fill.quantity,
        };
        let price = fill.price;
        let mut realized = Decimal::ZERO;
        let mut remove = false;

        {
            let position = self
                .positions
                .entry(fill.symbol.clone())
                .or_insert(Position {
                    symbol: fill.symbol.clone(),
                    quantity: Decimal::ZERO,
                    avg_entry_price: price,
                });

            if position.quantity.is_zero() {
                position.quantity = signed;
                position.avg_entry_price = price;
            } else if position.quantity.is_sign_positive() == signed.is_sign_positive() {
                // Adding to an existing position: weighted average entry.
                let old_abs = position.quantity.abs();
                let add_abs = signed.abs();
                let total = old_abs + add_abs;
                position.avg_entry_price =
                    (position.avg_entry_price * old_abs + price * add_abs) / total;
                position.quantity += signed;
            } else {
                // Reducing, closing, or flipping the position.
                let close_abs = position.quantity.abs().min(signed.abs());
                realized = if position.quantity.is_sign_positive() {
                    (price - position.avg_entry_price) * close_abs
                } else {
                    (position.avg_entry_price - price) * close_abs
                };

                let new_quantity = position.quantity + signed;
                if new_quantity.is_zero() {
                    position.quantity = Decimal::ZERO;
                    remove = true;
                } else if new_quantity.is_sign_positive() != position.quantity.is_sign_positive() {
                    // Flipped through zero; the remainder opens at the fill price.
                    position.quantity = new_quantity;
                    position.avg_entry_price = price;
                } else {
                    position.quantity = new_quantity;
                }
            }
        }

        if remove {
            self.positions.remove(&fill.symbol);
        }
        realized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Side;
    use chrono::Utc;

    fn fill(side: Side, quantity: i64, price: i64, fees: i64) -> Fill {
        Fill {
            timestamp: Utc::now(),
            symbol: "KO".to_string(),
            side,
            quantity: Decimal::from(quantity),
            price: Decimal::from(price),
            fees: Decimal::from(fees),
            slippage: Decimal::ZERO,
            reason: "test".to_string(),
            confidence: 0.5,
            source: "test".to_string(),
        }
    }

    #[test]
    fn long_round_trip_realizes_pnl() {
        let mut portfolio = Portfolio::new(Decimal::from(1_000));
        portfolio.apply_fill(&fill(Side::Buy, 10, 10, 0));
        assert_eq!(portfolio.cash, Decimal::from(900));
        assert_eq!(portfolio.position_qty("KO"), Decimal::from(10));

        portfolio.apply_fill(&fill(Side::Sell, 10, 12, 0));
        assert_eq!(portfolio.position_qty("KO"), Decimal::ZERO);
        assert_eq!(portfolio.realized_pnl, Decimal::from(20));
        assert_eq!(portfolio.trades[1].realized_pnl, Decimal::from(20));
        assert_eq!(portfolio.cash, Decimal::from(1_020));
    }

    #[test]
    fn short_round_trip_realizes_pnl() {
        let mut portfolio = Portfolio::new(Decimal::from(1_000));
        portfolio.apply_fill(&fill(Side::Sell, 5, 10, 0));
        assert_eq!(portfolio.cash, Decimal::from(1_050));

        portfolio.apply_fill(&fill(Side::Buy, 5, 8, 0));
        assert_eq!(portfolio.realized_pnl, Decimal::from(10));
        assert_eq!(portfolio.cash, Decimal::from(1_010));
        assert_eq!(portfolio.position_qty("KO"), Decimal::ZERO);
    }

    #[test]
    fn weighted_average_entry_price_is_used() {
        let mut portfolio = Portfolio::new(Decimal::from(1_000));
        portfolio.apply_fill(&fill(Side::Buy, 10, 10, 0));
        portfolio.apply_fill(&fill(Side::Buy, 10, 20, 0));
        let position = portfolio.position("KO").expect("position");
        assert_eq!(position.avg_entry_price, Decimal::from(15));
        assert_eq!(position.quantity, Decimal::from(20));
    }

    #[test]
    fn fees_reduce_net_pnl_but_not_gross_pnl() {
        let mut portfolio = Portfolio::new(Decimal::from(1_000));
        portfolio.apply_fill(&fill(Side::Buy, 10, 10, 2));
        portfolio.apply_fill(&fill(Side::Sell, 10, 12, 3));

        let prices = HashMap::new();
        assert_eq!(portfolio.realized_pnl, Decimal::from(20));
        assert_eq!(portfolio.fees_paid, Decimal::from(5));
        assert_eq!(portfolio.net_pnl(&prices), Decimal::from(15));
        assert_eq!(portfolio.gross_pnl(&prices), Decimal::from(20));
    }
}
