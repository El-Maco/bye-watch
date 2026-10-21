use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use engine::{decimal_from_f64, Candle, EngineError, FxSource, MarketData, Result};
use rust_decimal::Decimal;
use serde::Deserialize;

const DEFAULT_BASE_URL: &str = "https://query1.finance.yahoo.com";

pub struct YahooClient {
    client: reqwest::Client,
    base_url: String,
}

impl YahooClient {
    pub fn new() -> Result<Self> {
        Self::with_base_url(DEFAULT_BASE_URL)
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) bye-watch/0.1")
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| EngineError::MarketData(format!("failed to build HTTP client: {e}")))?;
        Ok(Self {
            client,
            base_url: base_url.into(),
        })
    }

    async fn fetch_chart(&self, symbol: &str, interval: &str, range: &str) -> Result<ChartData> {
        let encoded = urlencoding::encode(symbol);
        let url = format!(
            "{}/v8/finance/chart/{}?interval={}&range={}&events=div%2Csplits",
            self.base_url.trim_end_matches('/'),
            encoded,
            interval,
            range
        );
        self.request_chart(&url, symbol).await
    }

    async fn fetch_chart_period(
        &self,
        symbol: &str,
        interval: &str,
        period1: i64,
        period2: i64,
    ) -> Result<ChartData> {
        let encoded = urlencoding::encode(symbol);
        let url = format!(
            "{}/v8/finance/chart/{}?interval={}&period1={}&period2={}&events=div%2Csplits",
            self.base_url.trim_end_matches('/'),
            encoded,
            interval,
            period1,
            period2
        );
        self.request_chart(&url, symbol).await
    }

    async fn request_chart(&self, url: &str, symbol: &str) -> Result<ChartData> {
        let response =
            self.client.get(url).send().await.map_err(|e| {
                EngineError::MarketData(format!("request for {symbol} failed: {e}"))
            })?;

        if !response.status().is_success() {
            return Err(EngineError::MarketData(format!(
                "Yahoo returned HTTP {} for {symbol}",
                response.status()
            )));
        }

        let payload: ChartResponse = response.json().await.map_err(|e| {
            EngineError::MarketData(format!("invalid Yahoo response for {symbol}: {e}"))
        })?;

        if let Some(error) = payload.chart.error {
            let description = error
                .description
                .or(error.code)
                .unwrap_or_else(|| "unknown error".to_string());
            return Err(EngineError::MarketData(format!(
                "Yahoo error for {symbol}: {description}"
            )));
        }

        let mut results = payload
            .chart
            .result
            .ok_or_else(|| EngineError::MarketData(format!("no chart result for {symbol}")))?;
        if results.is_empty() {
            return Err(EngineError::MarketData(format!(
                "empty chart result for {symbol}"
            )));
        }
        let result = results.remove(0);

        let meta_price = result.meta.as_ref().and_then(|m| m.regular_market_price);
        let timestamps = result.timestamp.unwrap_or_default();
        let quote = result
            .indicators
            .quote
            .into_iter()
            .next()
            .unwrap_or_default();

        let mut candles = Vec::new();
        for (index, timestamp) in timestamps.iter().enumerate() {
            let close = match quote.close.get(index).copied().flatten() {
                Some(close) => close,
                None => continue,
            };
            let open = quote.open.get(index).copied().flatten().unwrap_or(close);
            let high = quote.high.get(index).copied().flatten().unwrap_or(close);
            let low = quote.low.get(index).copied().flatten().unwrap_or(close);
            let volume = quote.volume.get(index).copied().flatten().unwrap_or(0.0);
            candles.push(Candle {
                timestamp: *timestamp,
                open,
                high,
                low,
                close,
                volume,
            });
        }

        Ok(ChartData {
            candles,
            meta_price,
        })
    }

    async fn latest_price(&self, symbol: &str) -> Result<f64> {
        let chart = self.fetch_chart(symbol, "1m", "1d").await?;
        chart
            .candles
            .last()
            .map(|candle| candle.close)
            .or(chart.meta_price)
            .ok_or_else(|| EngineError::MarketData(format!("no price available for {symbol}")))
    }
}

struct ChartData {
    candles: Vec<Candle>,
    meta_price: Option<f64>,
}

#[async_trait]
impl MarketData for YahooClient {
    async fn candles(&self, symbol: &str, interval: &str, limit: usize) -> Result<Vec<Candle>> {
        let range = range_for(interval, limit);
        let chart = self.fetch_chart(symbol, interval, &range).await?;
        let mut candles = chart.candles;
        if candles.len() > limit {
            let start = candles.len() - limit;
            candles.drain(0..start);
        }
        if candles.len() < limit {
            tracing::debug!(
                symbol,
                interval,
                requested = limit,
                received = candles.len(),
                "Yahoo returned less history than requested"
            );
        }
        Ok(candles)
    }

    async fn history(
        &self,
        symbol: &str,
        interval: &str,
        from: Option<chrono::DateTime<Utc>>,
        to: Option<chrono::DateTime<Utc>>,
    ) -> Result<Vec<Candle>> {
        let period1 = from.map(|value| value.timestamp()).unwrap_or(0);
        let period2 = to
            .map(|value| value.timestamp())
            .unwrap_or_else(|| Utc::now().timestamp());

        let mut candles = self
            .fetch_chart_period(symbol, interval, period1, period2)
            .await?
            .candles;

        if let Some(from) = from {
            candles.retain(|candle| candle.timestamp >= from.timestamp());
        }
        if let Some(to) = to {
            candles.retain(|candle| candle.timestamp <= to.timestamp());
        }

        Ok(candles)
    }
}

#[async_trait]
impl FxSource for YahooClient {
    async fn rate(&self, from: &str, to: &str) -> Result<Decimal> {
        if from.eq_ignore_ascii_case(to) {
            return Ok(Decimal::ONE);
        }

        let direct = format!("{}{}=X", from.to_uppercase(), to.to_uppercase());
        match self.latest_price(&direct).await {
            Ok(price) if price > 0.0 => return decimal_from_f64(price),
            Ok(_) => {}
            Err(err) => tracing::debug!(pair = %direct, error = %err, "direct FX pair unavailable"),
        }

        let inverse = format!("{}{}=X", to.to_uppercase(), from.to_uppercase());
        match self.latest_price(&inverse).await {
            Ok(price) if price > 0.0 => {
                let rate = decimal_from_f64(price)?;
                return Ok(Decimal::ONE / rate);
            }
            Ok(_) => {}
            Err(err) => {
                tracing::debug!(pair = %inverse, error = %err, "inverse FX pair unavailable")
            }
        }

        Err(EngineError::MarketData(format!(
            "no FX rate available for {from}/{to}"
        )))
    }
}

fn range_for(interval: &str, limit: usize) -> String {
    match interval {
        "1m" => "7d".to_string(),
        "2m" | "5m" | "15m" | "30m" | "90m" => "60d".to_string(),
        "60m" | "1h" => "730d".to_string(),
        "1d" | "1wk" | "1mo" => if limit <= 250 {
            "1y"
        } else if limit <= 750 {
            "3y"
        } else if limit <= 1_250 {
            "5y"
        } else if limit <= 2_500 {
            "10y"
        } else {
            "max"
        }
        .to_string(),
        _ => "1y".to_string(),
    }
}

#[derive(Debug, Deserialize)]
struct ChartResponse {
    chart: Chart,
}

#[derive(Debug, Deserialize)]
struct Chart {
    result: Option<Vec<ChartResult>>,
    error: Option<ChartError>,
}

#[derive(Debug, Deserialize)]
struct ChartError {
    code: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChartResult {
    meta: Option<Meta>,
    timestamp: Option<Vec<i64>>,
    indicators: Indicators,
}

#[derive(Debug, Deserialize)]
struct Meta {
    #[serde(rename = "regularMarketPrice")]
    regular_market_price: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct Indicators {
    quote: Vec<Quote>,
}

#[derive(Debug, Deserialize, Default)]
struct Quote {
    #[serde(default)]
    open: Vec<Option<f64>>,
    #[serde(default)]
    high: Vec<Option<f64>>,
    #[serde(default)]
    low: Vec<Option<f64>>,
    #[serde(default)]
    close: Vec<Option<f64>>,
    #[serde(default)]
    volume: Vec<Option<f64>>,
}
