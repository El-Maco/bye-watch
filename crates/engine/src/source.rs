use std::collections::HashMap;

use async_trait::async_trait;

use crate::error::{EngineError, Result};
use crate::types::{SignalIntent, TickContext};

#[derive(Debug, Clone)]
pub struct SourceConfig {
    pub name: String,
    pub symbols: Vec<String>,
    pub interval: String,
    pub params: toml::Value,
}

#[async_trait]
pub trait SignalSource: Send + Sync {
    fn name(&self) -> &str;
    async fn on_tick(&mut self, ctx: &TickContext) -> Vec<SignalIntent>;
}

pub type SourceFactory = fn(SourceConfig) -> Result<Box<dyn SignalSource>>;

#[derive(Default)]
pub struct SourceRegistry {
    factories: HashMap<String, SourceFactory>,
}

impl SourceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, name: &str, factory: SourceFactory) {
        self.factories.insert(name.to_string(), factory);
    }

    pub fn build(&self, config: &SourceConfig) -> Result<Box<dyn SignalSource>> {
        let factory = self
            .factories
            .get(&config.name)
            .ok_or_else(|| EngineError::UnknownSource(config.name.clone()))?;
        factory(config.clone())
    }

    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.factories.keys().cloned().collect();
        names.sort();
        names
    }
}
