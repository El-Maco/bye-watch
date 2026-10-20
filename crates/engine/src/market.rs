use async_trait::async_trait;
use rust_decimal::Decimal;

use crate::error::Result;
use crate::types::Candle;

#[async_trait]
pub trait MarketData: Send + Sync {
    async fn candles(&self, symbol: &str, interval: &str, limit: usize) -> Result<Vec<Candle>>;
}

#[async_trait]
pub trait FxSource: Send + Sync {
    /// Returns how many units of `to` one unit of `from` is worth.
    async fn rate(&self, from: &str, to: &str) -> Result<Decimal>;
}
