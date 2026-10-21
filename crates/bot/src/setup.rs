use anyhow::bail;
use engine::{
    decimal_from_f64, EquityFractionSizer, FeeConfig, FeeModel, FixedSizer, PositionSizer,
    SourceRegistry,
};

use crate::config::Config;

pub(crate) fn build_fee_model(config: &Config) -> anyhow::Result<FeeModel> {
    Ok(FeeModel::from(FeeConfig {
        enabled: config.fees.enabled,
        commission_flat: decimal_from_f64(config.fees.commission_flat)?,
        commission_pct: decimal_from_f64(config.fees.commission_pct)?,
        slippage_bps: decimal_from_f64(config.fees.slippage_bps)?,
    }))
}

pub(crate) fn build_sizer(config: &Config) -> anyhow::Result<Box<dyn PositionSizer>> {
    let max_fraction = decimal_from_f64(config.sizing.max_fraction)?;
    let use_quantity_hint = config.sizing.use_quantity_hint;

    match config.sizing.mode.as_str() {
        "fixed" => Ok(Box::new(FixedSizer {
            quantity: decimal_from_f64(config.sizing.fixed_quantity)?,
            max_fraction,
            use_quantity_hint,
        })),
        "equity_fraction" => Ok(Box::new(EquityFractionSizer {
            fraction: decimal_from_f64(config.sizing.fraction)?,
            max_fraction,
            use_quantity_hint,
        })),
        other => bail!("unknown sizing.mode '{other}'"),
    }
}

pub(crate) fn build_registry() -> SourceRegistry {
    let mut registry = SourceRegistry::new();
    strategy_ma_cross::register(&mut registry);
    strategy_rsi::register(&mut registry);
    strategy_pairs::register(&mut registry);
    agent::register(&mut registry);
    registry
}
