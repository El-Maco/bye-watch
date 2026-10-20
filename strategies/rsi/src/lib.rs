use std::collections::HashMap;

use async_trait::async_trait;
use engine::{
    Candle, EngineError, Result, Side, SignalIntent, SignalSource, SourceConfig, SourceRegistry,
    TickContext,
};
use serde::Deserialize;

const NAME: &str = "rsi";

#[derive(Debug, Deserialize)]
struct Params {
    period: usize,
    buy_below: f64,
    sell_above: f64,
}

pub struct RsiStrategy {
    symbols: Vec<String>,
    interval: String,
    period: usize,
    buy_below: f64,
    sell_above: f64,
    last_seen: HashMap<String, i64>,
    zones: HashMap<String, i8>,
}

impl RsiStrategy {
    fn from_config(config: SourceConfig) -> Result<Self> {
        let params: Params = config
            .params
            .clone()
            .try_into()
            .map_err(|e| EngineError::Config(format!("{NAME} params: {e}")))?;
        if params.period == 0 {
            return Err(EngineError::Config(format!("{NAME} period must be > 0")));
        }
        if !(0.0 < params.buy_below
            && params.buy_below < params.sell_above
            && params.sell_above < 100.0)
        {
            return Err(EngineError::Config(format!(
                "{NAME} requires 0 < buy_below < sell_above < 100"
            )));
        }
        Ok(Self {
            symbols: config.symbols,
            interval: config.interval,
            period: params.period,
            buy_below: params.buy_below,
            sell_above: params.sell_above,
            last_seen: HashMap::new(),
            zones: HashMap::new(),
        })
    }
}

#[async_trait]
impl SignalSource for RsiStrategy {
    fn name(&self) -> &str {
        NAME
    }

    async fn on_tick(&mut self, ctx: &TickContext) -> Vec<SignalIntent> {
        let mut intents = Vec::new();
        let symbols = self.symbols.clone();

        for symbol in symbols {
            let candles = match ctx.candles_for(&symbol, &self.interval) {
                Some(candles) if candles.len() > self.period => candles,
                _ => continue,
            };

            let last_timestamp = match candles.last() {
                Some(candle) => candle.timestamp,
                None => continue,
            };
            if self.last_seen.get(&symbol).copied() == Some(last_timestamp) {
                continue;
            }

            let rsi = match rsi(candles, self.period) {
                Some(value) => value,
                None => continue,
            };

            let new_zone = if rsi < self.buy_below {
                -1
            } else if rsi > self.sell_above {
                1
            } else {
                0
            };
            let previous_zone = self.zones.get(&symbol).copied().unwrap_or(0);

            self.last_seen.insert(symbol.clone(), last_timestamp);
            self.zones.insert(symbol.clone(), new_zone);

            if new_zone == previous_zone {
                continue;
            }

            if new_zone == -1 {
                let confidence = (0.6 + (self.buy_below - rsi) / self.buy_below).clamp(0.55, 0.95);
                intents.push(SignalIntent::new(
                    symbol.clone(),
                    Side::Buy,
                    confidence,
                    format!("RSI {rsi:.1} below {:.1}", self.buy_below),
                    NAME,
                ));
            } else if new_zone == 1 {
                let span = (100.0 - self.sell_above).max(1.0);
                let confidence = (0.6 + (rsi - self.sell_above) / span).clamp(0.55, 0.95);
                intents.push(SignalIntent::closing(
                    symbol.clone(),
                    Side::Sell,
                    confidence,
                    format!("RSI {rsi:.1} above {:.1}", self.sell_above),
                    NAME,
                ));
            }
        }

        intents
    }
}

fn rsi(candles: &[Candle], period: usize) -> Option<f64> {
    if period == 0 || candles.len() < period + 1 {
        return None;
    }
    let slice = &candles[candles.len() - period - 1..];
    let mut gains = 0.0;
    let mut losses = 0.0;
    for window in slice.windows(2) {
        let difference = window[1].close - window[0].close;
        if difference > 0.0 {
            gains += difference;
        } else {
            losses -= difference;
        }
    }

    let average_gain = gains / period as f64;
    let average_loss = losses / period as f64;

    if average_loss == 0.0 {
        return Some(if average_gain == 0.0 { 50.0 } else { 100.0 });
    }
    let relative_strength = average_gain / average_loss;
    Some(100.0 - 100.0 / (1.0 + relative_strength))
}

pub fn register(registry: &mut SourceRegistry) {
    registry.register(NAME, |config| {
        RsiStrategy::from_config(config).map(|strategy| Box::new(strategy) as Box<dyn SignalSource>)
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

    fn strategy() -> RsiStrategy {
        RsiStrategy {
            symbols: vec!["KO".to_string()],
            interval: "1d".to_string(),
            period: 2,
            buy_below: 30.0,
            sell_above: 70.0,
            last_seen: HashMap::new(),
            zones: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn emits_buy_when_entering_oversold() {
        let mut strategy = strategy();
        let closes = [10.0, 9.0, 8.0];
        let candles = closes
            .iter()
            .enumerate()
            .map(|(index, close)| candle(index as i64, *close))
            .collect();

        let mut ctx = TickContext::default();
        ctx.candles.insert(MarketKey::new("KO", "1d"), candles);

        let intents = strategy.on_tick(&ctx).await;
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].direction, Side::Buy);
    }

    #[tokio::test]
    async fn emits_sell_when_entering_overbought() {
        let mut strategy = strategy();
        let closes = [10.0, 11.0, 12.0];
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
    async fn does_not_repeat_signal_inside_zone() {
        let mut strategy = strategy();
        let closes = [10.0, 9.0, 8.0, 7.0];
        let candles = closes
            .iter()
            .enumerate()
            .map(|(index, close)| candle(index as i64, *close))
            .collect();

        let mut ctx = TickContext::default();
        ctx.candles.insert(MarketKey::new("KO", "1d"), candles);

        assert_eq!(strategy.on_tick(&ctx).await.len(), 1);
        let mut next = strategy.on_tick(&ctx).await;
        // Same candle is ignored; a new candle deeper in the zone also stays silent.
        assert!(next.is_empty());

        let mut ctx2 = TickContext::default();
        ctx2.candles.insert(
            MarketKey::new("KO", "1d"),
            [10.0, 9.0, 8.0, 7.0, 6.0]
                .iter()
                .enumerate()
                .map(|(index, close)| candle(index as i64, *close))
                .collect(),
        );
        next = strategy.on_tick(&ctx2).await;
        assert!(next.is_empty());
    }
}
