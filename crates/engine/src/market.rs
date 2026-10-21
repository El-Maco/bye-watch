use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::error::Result;
use crate::types::Candle;

#[async_trait]
pub trait MarketData: Send + Sync {
    async fn candles(&self, symbol: &str, interval: &str, limit: usize) -> Result<Vec<Candle>>;

    /// Historical candles bounded by optional timestamps. `None` means "as far
    /// back as the provider allows" for `from` and "now" for `to`.
    async fn history(
        &self,
        symbol: &str,
        interval: &str,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Result<Vec<Candle>>;
}

#[async_trait]
pub trait FxSource: Send + Sync {
    /// Returns how many units of `to` one unit of `from` is worth.
    async fn rate(&self, from: &str, to: &str) -> Result<Decimal>;
}
