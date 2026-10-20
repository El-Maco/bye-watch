# bye-watch

An async Rust bot for equities that runs configurable strategies, executes
trades in a paper portfolio, and reports every action so you can place the same
trades manually if you want to.

Default universe: Coca-Cola (`KO`) and PepsiCo (`PEP`). Default ledger: **EUR**,
starting at **€100**. Default data source: the keyless Yahoo Finance chart API.

## What it does

1. Fetches candles and the USD/EUR rate from Yahoo Finance.
2. Runs each configured strategy in its own crate.
3. Ranks signals by confidence and keeps the best signal per symbol.
4. Sizes the order with a fixed or equity-fraction policy, capped at `max_fraction`
   and never all-in.
5. Executes a simulated order with commission and slippage.
6. Records the trade, persists `portfolio.json`, and prints/logs a report.

No live broker is implemented yet. The `Broker` trait is the seam for one.

## Workspace layout

```
crates/
  engine/          shared types, traits, registry, portfolio, fees, paper broker, sizers
  bot/             async binary: config, scheduler, aggregation, execution, reporting
  market-yahoo/    keyless Yahoo Finance market data and FX adapter
  social/          SocialProvider trait and the default no-op provider
  agent/           optional OpenAI-compatible LLM signal source
strategies/
  moving-average-cross/   ma_cross
  rsi/                    rsi
  pairs-trading/          pairs_trading (KO/PEP)
```

A strategy is a separate library crate. The bot only knows the
`SignalSource` trait and the compile-time registry.

## Quick start

```bash
cp config.example.toml config.toml
cargo run -p bye-watch-bot --release -- --config config.toml
```

The bot creates `logs/bye-watch.log.YYYY-MM-DD` and writes `portfolio.json` when
it trades. Stop it with Ctrl-C; the portfolio is saved on exit.

Run validation:

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```

## Configuration

See `config.example.toml` for every option. The most important ones:

| Section | Key | Meaning |
| --- | --- | --- |
| `market` | `provider` | `yahoo` today; the trait allows keyed providers later |
| `market` | `instrument_currency` / `quote_currency` | USD prices converted into a EUR ledger |
| `market` | `fx_fallback_rate` | EUR per USD used if the live lookup fails |
| `paper` | `starting_cash_eur` | Starting paper cash |
| `paper` | `state_path` | Persisted portfolio JSON |
| `paper` | `allow_short` | Required for pairs trading |
| `fees` | `enabled` | Toggle all trading costs in fills and PnL |
| `fees` | `commission_flat`, `commission_pct`, `slippage_bps` | Cost model |
| `sizing` | `mode` | `equity_fraction` or `fixed` |
| `sizing` | `max_fraction` | Hard cap, must be `< 1`; never all-in |
| `engine` | `tick_secs` | Loop cadence |
| `engine` | `max_history` | Candles fetched per symbol/interval |
| `engine` | `market_hours_only` | Gate intraday runs to US market hours |
| `engine` | `log_heartbeat_secs` | Minimum interval between idle tick summaries |
| `agent` | `enabled` | Optional LLM source |
| `agent` | `base_url` / `model` | OpenAI-compatible endpoint and model |

Valid intervals: `1m`, `2m`, `5m`, `15m`, `30m`, `60m`, `1h`, `1d`, `1wk`,
`1mo`. Strategies are interval-agnostic, but indicator periods must be tuned for
the interval you choose. Yahoo limits history for intraday data (about 7 days
for `1m`, 60 days for `5m`–`30m`, 730 days for `1h`).

## Strategies

### `ma_cross`

Golden cross buys, death cross closes the long position.

```toml
params = { fast = 50, slow = 200 }
```

### `rsi`

Enters when RSI crosses into oversold, exits when it crosses into overbought.
Signals fire on zone changes only, not every candle.

```toml
params = { period = 14, buy_below = 30, sell_above = 70 }
```

### `pairs_trading`

Trades the log-price spread of two symbols with a rolling z-score. When
`z >= entry_z` it sells the first leg and buys the second; when
`z <= -entry_z` it does the reverse. Positions close when the spread reverts
inside `exit_z`. Short selling must be enabled.

```toml
params = { lookback = 60, entry_z = 2.0, exit_z = 0.5, beta = 1.0 }
```

## Adding a strategy

1. Create `strategies/my-strategy` as a library crate depending on `engine`.
2. Implement `SignalSource`:
   ```rust
   async fn on_tick(&mut self, ctx: &TickContext) -> Vec<SignalIntent>
   ```
3. Export `pub fn register(registry: &mut SourceRegistry)`.
4. Add the crate to the workspace and call `my_strategy::register(&mut registry)`
   in `crates/bot/src/runner.rs`.
5. Add a `[[strategies]]` entry with `name = "my_strategy"`.

Strategies read candles from `TickContext` and must track the last candle they
evaluated so they do not repeat a signal when the bot polls faster than the
interval.

## Fees and PnL

Fills apply `slippage_bps` and `commission_pct` plus `commission_flat`. Reports
show **net PnL** (after costs), **gross PnL** (commission and slippage added
back), and the two cost components separately. Set `fees.enabled = false` to
ignore costs entirely.

A realistic default for US retail is zero commission with a few basis points of
spread. EU brokers often charge a flat fee or about 0.25%. With a €100 account,
flat fees matter a lot, so adjust the preset to match your broker.

## Sizing and future LLM sizing

`equity_fraction` allocates `fraction` of current equity per signal, capped by
`max_fraction`. `fixed` uses `fixed_quantity`, also capped by `max_fraction`.
The engine clamps the effective fraction to `0.95`, so an all-in order is
impossible even with a bad config.

Signals may carry `quantity_hint`. Set `sizing.use_quantity_hint = true` to let a
source cap the size, which is the hook for an LLM deciding position size later.

## LLM and social signals

`crates/agent` implements `llm_agent`. When enabled it periodically sends recent
closes plus any social posts to an OpenAI-compatible endpoint and expects:

```json
{"signals":[{"symbol":"KO","direction":"buy","confidence":0.7,"reason":"...","quantity_hint":null}]}
```

The default endpoint is `http://localhost:8787/v1` with
`deepseek-v4.1-flash`. If the proxy is down, returns bad JSON, or suggests a
symbol outside the configured universe, the agent retries `retry_attempts`
times, logs a warning, backs off for `failure_cooldown_secs`, and returns no
signals. The rest of the bot keeps running.

`crates/social` currently ships only `NullSocialProvider`. The `SocialProvider`
trait is the seam for a real feed later. An unconfigured provider never breaks
the run.

## Data source notes

The Yahoo Finance chart endpoint is keyless but unofficial and may rate-limit.
Market data is behind the `MarketData` trait, and FX is behind `FxSource`, so a
keyed provider can be added without touching strategies or the engine. Market
holidays and early closes are not modelled; the scheduler only checks weekday
US market hours.

## Live trading

`Broker` is implemented only by `PaperBroker`. A live adapter would sign and
submit orders through the same trait, then the bot could switch via config. Do
not add live execution without explicit risk controls and confirmation.
