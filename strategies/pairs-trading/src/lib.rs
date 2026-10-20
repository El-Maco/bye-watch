use std::collections::HashMap;

use async_trait::async_trait;
use engine::{
    Candle, EngineError, Result, Side, SignalIntent, SignalSource, SourceConfig, SourceRegistry,
    TickContext,
};
use serde::Deserialize;

const NAME: &str = "pairs_trading";

#[derive(Debug, Deserialize)]
struct Params {
    lookback: usize,
    entry_z: f64,
    exit_z: f64,
    #[serde(default = "default_beta")]
    beta: f64,
}

fn default_beta() -> f64 {
    1.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PairState {
    Flat,
    LongSpread,
    ShortSpread,
}

pub struct PairsTrading {
    symbols: Vec<String>,
    interval: String,
    lookback: usize,
    entry_z: f64,
    exit_z: f64,
    beta: f64,
    state: PairState,
    last_seen: Option<(i64, i64)>,
}

impl PairsTrading {
    fn from_config(config: SourceConfig) -> Result<Self> {
        if config.symbols.len() < 2 {
            return Err(EngineError::Config(format!(
                "{NAME} requires exactly two symbols"
            )));
        }
        let params: Params = config
            .params
            .clone()
            .try_into()
            .map_err(|e| EngineError::Config(format!("{NAME} params: {e}")))?;
        if params.lookback < 2 {
            return Err(EngineError::Config(format!(
                "{NAME} lookback must be at least 2"
            )));
        }
        if params.entry_z <= 0.0 || params.exit_z < 0.0 || params.exit_z >= params.entry_z {
            return Err(EngineError::Config(format!(
                "{NAME} requires 0 <= exit_z < entry_z"
            )));
        }
        if params.beta <= 0.0 {
            return Err(EngineError::Config(format!("{NAME} beta must be positive")));
        }
        Ok(Self {
            symbols: config.symbols,
            interval: config.interval,
            lookback: params.lookback,
            entry_z: params.entry_z,
            exit_z: params.exit_z,
            beta: params.beta,
            state: PairState::Flat,
            last_seen: None,
        })
    }
}

#[async_trait]
impl SignalSource for PairsTrading {
    fn name(&self) -> &str {
        NAME
    }

    async fn on_tick(&mut self, ctx: &TickContext) -> Vec<SignalIntent> {
        let symbol_a = match self.symbols.first() {
            Some(symbol) => symbol.clone(),
            None => return Vec::new(),
        };
        let symbol_b = match self.symbols.get(1) {
            Some(symbol) => symbol.clone(),
            None => return Vec::new(),
        };

        let candles_a = match ctx.candles_for(&symbol_a, &self.interval) {
            Some(candles) if candles.len() >= self.lookback => candles,
            _ => return Vec::new(),
        };
        let candles_b = match ctx.candles_for(&symbol_b, &self.interval) {
            Some(candles) if candles.len() >= self.lookback => candles,
            _ => return Vec::new(),
        };

        let last_timestamp = match (candles_a.last(), candles_b.last()) {
            (Some(a), Some(b)) => (a.timestamp, b.timestamp),
            _ => return Vec::new(),
        };
        if self.last_seen == Some(last_timestamp) {
            return Vec::new();
        }

        let aligned = aligned_closes(candles_a, candles_b);
        if aligned.len() < self.lookback {
            return Vec::new();
        }
        let window = &aligned[aligned.len() - self.lookback..];
        let z = match zscore(window, self.beta) {
            Some(z) if z.is_finite() => z,
            _ => return Vec::new(),
        };

        self.last_seen = Some(last_timestamp);
        let mut intents = Vec::new();

        match self.state {
            PairState::Flat => {
                if z >= self.entry_z {
                    let confidence = confidence(z, self.entry_z);
                    intents.push(SignalIntent::new(
                        symbol_a,
                        Side::Sell,
                        confidence,
                        format!("short spread z={z:.2}"),
                        NAME,
                    ));
                    intents.push(SignalIntent::new(
                        symbol_b,
                        Side::Buy,
                        confidence,
                        format!("long hedge leg z={z:.2}"),
                        NAME,
                    ));
                    self.state = PairState::ShortSpread;
                } else if z <= -self.entry_z {
                    let confidence = confidence(z, self.entry_z);
                    intents.push(SignalIntent::new(
                        symbol_a,
                        Side::Buy,
                        confidence,
                        format!("long spread z={z:.2}"),
                        NAME,
                    ));
                    intents.push(SignalIntent::new(
                        symbol_b,
                        Side::Sell,
                        confidence,
                        format!("short hedge leg z={z:.2}"),
                        NAME,
                    ));
                    self.state = PairState::LongSpread;
                }
            }
            PairState::LongSpread => {
                if z >= -self.exit_z {
                    intents.push(SignalIntent::closing(
                        symbol_a,
                        Side::Sell,
                        0.9,
                        format!("exit long spread z={z:.2}"),
                        NAME,
                    ));
                    intents.push(SignalIntent::closing(
                        symbol_b,
                        Side::Buy,
                        0.9,
                        format!("exit short hedge z={z:.2}"),
                        NAME,
                    ));
                    self.state = PairState::Flat;
                }
            }
            PairState::ShortSpread => {
                if z <= self.exit_z {
                    intents.push(SignalIntent::closing(
                        symbol_a,
                        Side::Buy,
                        0.9,
                        format!("exit short spread z={z:.2}"),
                        NAME,
                    ));
                    intents.push(SignalIntent::closing(
                        symbol_b,
                        Side::Sell,
                        0.9,
                        format!("exit long hedge z={z:.2}"),
                        NAME,
                    ));
                    self.state = PairState::Flat;
                }
            }
        }

        intents
    }
}

fn aligned_closes(a: &[Candle], b: &[Candle]) -> Vec<(f64, f64)> {
    let by_timestamp: HashMap<i64, f64> = b
        .iter()
        .map(|candle| (candle.timestamp, candle.close))
        .collect();
    a.iter()
        .filter_map(|candle| {
            by_timestamp
                .get(&candle.timestamp)
                .map(|other| (candle.close, *other))
        })
        .collect()
}

fn zscore(pairs: &[(f64, f64)], beta: f64) -> Option<f64> {
    if pairs.len() < 2 {
        return None;
    }
    let spreads: Vec<f64> = pairs.iter().map(|(a, b)| a.ln() - beta * b.ln()).collect();
    let count = spreads.len() as f64;
    let mean = spreads.iter().sum::<f64>() / count;
    let variance = spreads
        .iter()
        .map(|spread| (spread - mean).powi(2))
        .sum::<f64>()
        / (count - 1.0);
    let standard_deviation = variance.sqrt();
    if standard_deviation <= f64::EPSILON {
        return None;
    }
    Some((spreads[spreads.len() - 1] - mean) / standard_deviation)
}

fn confidence(z: f64, entry_z: f64) -> f64 {
    let excess = (z.abs() - entry_z).max(0.0);
    (0.65 + (excess / entry_z.max(0.1)) * 0.25).clamp(0.6, 0.95)
}

pub fn register(registry: &mut SourceRegistry) {
    registry.register(NAME, |config| {
        PairsTrading::from_config(config)
            .map(|strategy| Box::new(strategy) as Box<dyn SignalSource>)
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

    fn strategy() -> PairsTrading {
        PairsTrading {
            symbols: vec!["KO".to_string(), "PEP".to_string()],
            interval: "1d".to_string(),
            lookback: 20,
            entry_z: 1.5,
            exit_z: 0.5,
            beta: 1.0,
            state: PairState::Flat,
            last_seen: None,
        }
    }

    fn context(ko_last: f64) -> TickContext {
        let mut ko = Vec::new();
        let mut pep = Vec::new();
        for index in 0..20 {
            ko.push(candle(index as i64, 100.0));
            pep.push(candle(index as i64, 100.0));
        }
        ko.push(candle(20, ko_last));
        pep.push(candle(20, 100.0));

        let mut ctx = TickContext::default();
        ctx.candles.insert(MarketKey::new("KO", "1d"), ko);
        ctx.candles.insert(MarketKey::new("PEP", "1d"), pep);
        ctx
    }

    fn reverted_context() -> TickContext {
        let mut ko = Vec::new();
        let mut pep = Vec::new();
        for index in 0..19 {
            let price = if index % 2 == 0 { 95.0 } else { 105.0 };
            ko.push(candle(index as i64, price));
            pep.push(candle(index as i64, 100.0));
        }
        ko.push(candle(19, 100.0));
        pep.push(candle(19, 100.0));

        let mut ctx = TickContext::default();
        ctx.candles.insert(MarketKey::new("KO", "1d"), ko);
        ctx.candles.insert(MarketKey::new("PEP", "1d"), pep);
        ctx
    }

    #[tokio::test]
    async fn enters_short_spread_on_high_zscore() {
        let mut strategy = strategy();
        let intents = strategy.on_tick(&context(130.0)).await;
        assert_eq!(intents.len(), 2);
        assert_eq!(intents[0].symbol, "KO");
        assert_eq!(intents[0].direction, Side::Sell);
        assert_eq!(intents[1].symbol, "PEP");
        assert_eq!(intents[1].direction, Side::Buy);
        assert_eq!(strategy.state, PairState::ShortSpread);
    }

    #[tokio::test]
    async fn enters_long_spread_on_low_zscore() {
        let mut strategy = strategy();
        let intents = strategy.on_tick(&context(75.0)).await;
        assert_eq!(intents.len(), 2);
        assert_eq!(intents[0].direction, Side::Buy);
        assert_eq!(intents[1].direction, Side::Sell);
        assert_eq!(strategy.state, PairState::LongSpread);
    }

    #[tokio::test]
    async fn exits_when_spread_reverts() {
        let mut strategy = strategy();
        strategy.state = PairState::LongSpread;
        let intents = strategy.on_tick(&reverted_context()).await;
        assert_eq!(intents.len(), 2);
        assert!(intents[0].close_position);
        assert!(intents[1].close_position);
        assert_eq!(strategy.state, PairState::Flat);
    }
}
