use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::error::{EngineError, Result};

pub fn decimal_from_f64(value: f64) -> Result<Decimal> {
    if !value.is_finite() {
        return Err(EngineError::Decimal(format!("non-finite value: {value}")));
    }
    Decimal::from_f64_retain(value)
        .map(|d| d.round_dp(6))
        .ok_or_else(|| EngineError::Decimal(format!("cannot represent {value} as Decimal")))
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MarketKey {
    pub symbol: String,
    pub interval: String,
}

impl MarketKey {
    pub fn new(symbol: impl Into<String>, interval: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            interval: interval.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Candle {
    pub timestamp: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

impl Candle {
    pub fn close_decimal(&self) -> Result<Decimal> {
        decimal_from_f64(self.close)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Side::Buy => "buy",
            Side::Sell => "sell",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SignalIntent {
    pub symbol: String,
    pub direction: Side,
    pub confidence: f64,
    pub reason: String,
    pub source: String,
    /// When true the sizer returns the absolute size of the current position,
    /// so the order closes rather than reverses or opens a new position.
    pub close_position: bool,
    pub quantity_hint: Option<Decimal>,
}

impl SignalIntent {
    pub fn new(
        symbol: impl Into<String>,
        direction: Side,
        confidence: f64,
        reason: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            symbol: symbol.into(),
            direction,
            confidence: confidence.clamp(0.0, 1.0),
            reason: reason.into(),
            source: source.into(),
            close_position: false,
            quantity_hint: None,
        }
    }

    pub fn closing(
        symbol: impl Into<String>,
        direction: Side,
        confidence: f64,
        reason: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        let mut intent = Self::new(symbol, direction, confidence, reason, source);
        intent.close_position = true;
        intent
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PositionView {
    pub quantity: Decimal,
    pub avg_entry_price: Decimal,
}

#[derive(Debug, Clone, Default)]
pub struct PortfolioView {
    pub cash: Decimal,
    pub equity: Decimal,
    pub positions: HashMap<String, PositionView>,
}

impl PortfolioView {
    pub fn position_qty(&self, symbol: &str) -> Decimal {
        self.positions
            .get(symbol)
            .map(|p| p.quantity)
            .unwrap_or(Decimal::ZERO)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialPost {
    pub source: String,
    pub author: String,
    pub text: String,
    pub timestamp: DateTime<Utc>,
    pub url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TickContext {
    pub now: DateTime<Utc>,
    pub candles: HashMap<MarketKey, Vec<Candle>>,
    /// Latest price per symbol converted into the portfolio quote currency.
    pub prices: HashMap<String, Decimal>,
    pub portfolio: PortfolioView,
    pub social: Vec<SocialPost>,
}

impl Default for TickContext {
    fn default() -> Self {
        Self {
            now: Utc::now(),
            candles: HashMap::new(),
            prices: HashMap::new(),
            portfolio: PortfolioView::default(),
            social: Vec::new(),
        }
    }
}

impl TickContext {
    pub fn candles_for(&self, symbol: &str, interval: &str) -> Option<&[Candle]> {
        self.candles
            .get(&MarketKey::new(symbol, interval))
            .map(|candles| candles.as_slice())
    }

    pub fn last_candle(&self, symbol: &str, interval: &str) -> Option<&Candle> {
        self.candles_for(symbol, interval).and_then(|c| c.last())
    }
}
