pub mod broker;
pub mod error;
pub mod fees;
pub mod market;
pub mod notifier;
pub mod portfolio;
pub mod sizer;
pub mod social;
pub mod source;
pub mod types;

pub use broker::{Broker, OrderRequest, PaperBroker};
pub use error::{EngineError, Result};
pub use fees::{FeeConfig, FeeModel};
pub use market::{FxSource, MarketData};
pub use notifier::{
    ActionReport, ConsoleNotifier, Notifier, PortfolioSummary, PositionSummary, Report,
};
pub use portfolio::{Fill, Portfolio, Position, Trade};
pub use sizer::{EquityFractionSizer, FixedSizer, PositionSizer};
pub use social::SocialProvider;
pub use source::{SignalSource, SourceConfig, SourceFactory, SourceRegistry};
pub use types::{
    decimal_from_f64, Candle, MarketKey, PortfolioView, PositionView, Side, SignalIntent,
    SocialPost, TickContext,
};
