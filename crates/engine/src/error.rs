use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("unknown signal source: {0}")]
    UnknownSource(String),
    #[error("invalid configuration: {0}")]
    Config(String),
    #[error("market data error: {0}")]
    MarketData(String),
    #[error("broker error: {0}")]
    Broker(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("toml error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("decimal conversion error: {0}")]
    Decimal(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, EngineError>;
