use std::collections::HashMap;

use chrono::{DateTime, Utc};
use engine::{
    ActionReport, Broker, OrderRequest, Portfolio, PortfolioView, PositionSizer, PositionView,
    Side, SignalIntent,
};
use rust_decimal::Decimal;

pub(crate) fn portfolio_view(
    portfolio: &Portfolio,
    prices: &HashMap<String, Decimal>,
) -> PortfolioView {
    let mut positions = HashMap::new();
    for (symbol, position) in &portfolio.positions {
        positions.insert(
            symbol.clone(),
            PositionView {
                quantity: position.quantity,
                avg_entry_price: position.avg_entry_price,
            },
        );
    }
    PortfolioView {
        cash: portfolio.cash,
        equity: portfolio.equity(prices),
        positions,
    }
}

/// Ranks intents by confidence per symbol, sizes them, and sends them to the
/// broker. Returns the executed actions and the reasons any were skipped.
pub(crate) async fn execute_intents(
    broker: &mut dyn Broker,
    sizer: &dyn PositionSizer,
    intents: Vec<SignalIntent>,
    prices: &HashMap<String, Decimal>,
    now: DateTime<Utc>,
) -> (Vec<ActionReport>, Vec<String>) {
    let mut best: HashMap<String, SignalIntent> = HashMap::new();
    for intent in intents {
        let replace = match best.get(&intent.symbol) {
            Some(existing) => intent.confidence > existing.confidence,
            None => true,
        };
        if replace {
            best.insert(intent.symbol.clone(), intent);
        }
    }

    let mut actions = Vec::new();
    let mut skipped = Vec::new();

    for intent in best.into_values() {
        let price = match prices.get(&intent.symbol).copied() {
            Some(price) if price > Decimal::ZERO => price,
            _ => {
                skipped.push(format!("{}: no price available", intent.symbol));
                continue;
            }
        };

        let snapshot = portfolio_view(broker.portfolio(), prices);
        let quantity = match sizer.size(&intent, &snapshot, prices) {
            Ok(quantity) => quantity,
            Err(err) => {
                skipped.push(format!("{}: sizing failed: {err}", intent.symbol));
                continue;
            }
        };

        let quantity = if intent.direction == Side::Buy && !intent.close_position {
            let cash = broker.portfolio().cash;
            quantity.min((cash / price).round_dp(6))
        } else {
            quantity
        };

        if quantity <= Decimal::ZERO {
            skipped.push(format!(
                "{}: no size (close_position={}, direction={})",
                intent.symbol,
                intent.close_position,
                intent.direction.as_str()
            ));
            continue;
        }

        let order = OrderRequest {
            symbol: intent.symbol.clone(),
            side: intent.direction,
            quantity,
            reference_price: price,
            reason: intent.reason.clone(),
            confidence: intent.confidence,
            source: intent.source.clone(),
            timestamp: Some(now),
        };

        match broker.place_order(&order).await {
            Ok(fill) => actions.push(ActionReport {
                timestamp: fill.timestamp,
                symbol: fill.symbol,
                side: fill.side,
                quantity: fill.quantity,
                price: fill.price,
                fees: fill.fees,
                confidence: fill.confidence,
                source: fill.source,
                reason: fill.reason,
            }),
            Err(err) => skipped.push(format!("{}: order rejected: {err}", order.symbol)),
        }
    }

    (actions, skipped)
}
