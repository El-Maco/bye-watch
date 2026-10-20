use std::collections::HashMap;

use async_trait::async_trait;
use engine::{
    Candle, EngineError, Result, Side, SignalIntent, SignalSource, SourceConfig, SourceRegistry,
    TickContext,
};
use serde::Deserialize;

const NAME: &str = "ma_cross";

#[derive(Debug, Deserialize)]
struct Params {
    fast: usize,
    slow: usize,
}

pub struct MaCross {
    symbols: Vec<String>,
    interval: String,
    fast: usize,
    slow: usize,
    last_seen: HashMap<String, i64>,
}

impl MaCross {
    fn from_config(config: SourceConfig) -> Result<Self> {
        let params: Params = config
            .params
            .clone()
            .try_into()
            .map_err(|e| EngineError::Config(format!("{NAME} params: {e}")))?;
        if params.fast == 0 || params.slow == 0 || params.fast >= params.slow {
            return Err(EngineError::Config(format!(
                "{NAME} requires 0 < fast < slow"
            )));
        }
        Ok(Self {
            symbols: config.symbols,
            interval: config.interval,
            fast: params.fast,
            slow: params.slow,
            last_seen: HashMap::new(),
        })
    }
}

#[async_trait]
impl SignalSource for MaCross {
    fn name(&self) -> &str {
        NAME
    }

    async fn on_tick(&mut self, ctx: &TickContext) -> Vec<SignalIntent> {
        let mut intents = Vec::new();
        let symbols = self.symbols.clone();

        for symbol in symbols {
            let candles = match ctx.candles_for(&symbol, &self.interval) {
                Some(candles) if candles.len() > self.slow => candles,
                _ => continue,
            };

            let last_timestamp = match candles.last() {
                Some(candle) => candle.timestamp,
                None => continue,
            };
            if self.last_seen.get(&symbol).copied() == Some(last_timestamp) {
                continue;
            }

            let fast_now = match sma(candles, self.fast, 0) {
                Some(value) => value,
                None => continue,
            };
            let slow_now = match sma(candles, self.slow, 0) {
                Some(value) => value,
                None => continue,
            };
            let fast_prev = match sma(candles, self.fast, 1) {
                Some(value) => value,
                None => continue,
            };
            let slow_prev = match sma(candles, self.slow, 1) {
                Some(value) => value,
                None => continue,
            };

            self.last_seen.insert(symbol.clone(), last_timestamp);

            if fast_prev <= slow_prev && fast_now > slow_now {
                intents.push(SignalIntent::new(
                    symbol.clone(),
                    Side::Buy,
                    confidence(fast_now, slow_now),
                    format!("golden cross fast={fast_now:.4} slow={slow_now:.4}"),
                    NAME,
                ));
            } else if fast_prev >= slow_prev && fast_now < slow_now {
                intents.push(SignalIntent::closing(
                    symbol.clone(),
                    Side::Sell,
                    confidence(fast_now, slow_now),
                    format!("death cross fast={fast_now:.4} slow={slow_now:.4}"),
                    NAME,
                ));
            }
        }

        intents
    }
}

fn sma(candles: &[Candle], period: usize, end_offset: usize) -> Option<f64> {
    let end = candles.len().checked_sub(end_offset)?;
    if period == 0 || end < period {
        return None;
    }
    let slice = &candles[end - period..end];
    let sum: f64 = slice.iter().map(|candle| candle.close).sum();
    Some(sum / period as f64)
}

fn confidence(fast: f64, slow: f64) -> f64 {
    if slow == 0.0 {
        return 0.6;
    }
    let separation = ((fast - slow) / slow).abs();
    (0.6 + separation * 10.0).clamp(0.55, 0.95)
}

pub fn register(registry: &mut SourceRegistry) {
    registry.register(NAME, |config| {
        MaCross::from_config(config).map(|strategy| Box::new(strategy) as Box<dyn SignalSource>)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::MarketKey;

    fn candle(timestamp: i64, close: f64) -> Candle {
        Candle {
            timestamp,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.0,
        }
    }

    fn strategy() -> MaCross {
        MaCross {
            symbols: vec!["KO".to_string()],
            interval: "1d".to_string(),
            fast: 2,
            slow: 3,
            last_seen: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn emits_buy_on_golden_cross() {
        let mut strategy = strategy();
        let closes = [10.0, 10.0, 10.0, 10.0, 10.0, 20.0];
        let candles = closes
            .iter()
            .enumerate()
            .map(|(index, close)| candle(index as i64, *close))
            .collect();

        let mut ctx = TickContext::default();
        ctx.candles.insert(MarketKey::new("KO", "1d"), candles);

        let intents = strategy.on_tick(&ctx).await;
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].symbol, "KO");
        assert_eq!(intents[0].direction, Side::Buy);
        assert!(!intents[0].close_position);
    }

    #[tokio::test]
    async fn emits_sell_on_death_cross() {
        let mut strategy = strategy();
        let closes = [20.0, 20.0, 20.0, 20.0, 20.0, 10.0];
        let candles = closes
            .iter()
            .enumerate()
            .map(|(index, close)| candle(index as i64, *close))
            .collect();

        let mut ctx = TickContext::default();
        ctx.candles.insert(MarketKey::new("KO", "1d"), candles);

        let intents = strategy.on_tick(&ctx).await;
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].direction, Side::Sell);
        assert!(intents[0].close_position);
    }

    #[tokio::test]
    async fn ignores_repeated_candle() {
        let mut strategy = strategy();
        let closes = [10.0, 10.0, 10.0, 10.0, 10.0, 20.0];
        let candles = closes
            .iter()
            .enumerate()
            .map(|(index, close)| candle(index as i64, *close))
            .collect();

        let mut ctx = TickContext::default();
        ctx.candles.insert(MarketKey::new("KO", "1d"), candles);

        assert_eq!(strategy.on_tick(&ctx).await.len(), 1);
        assert_eq!(strategy.on_tick(&ctx).await.len(), 0);
    }
}
