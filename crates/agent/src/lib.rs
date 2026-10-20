use std::time::{Duration, Instant};

use async_trait::async_trait;
use engine::{
    decimal_from_f64, EngineError, Result, Side, SignalIntent, SignalSource, SourceConfig,
    SourceRegistry, TickContext,
};
use serde::{Deserialize, Serialize};

const SOURCE_NAME: &str = "llm_agent";

#[derive(Debug, Deserialize)]
struct AgentParams {
    base_url: String,
    model: String,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default = "default_min_interval")]
    min_interval_secs: u64,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
    #[serde(default = "default_temperature")]
    temperature: f64,
    #[serde(default = "default_retry_attempts")]
    retry_attempts: u32,
    #[serde(default = "default_failure_cooldown")]
    failure_cooldown_secs: u64,
}

fn default_min_interval() -> u64 {
    3_600
}

fn default_timeout() -> u64 {
    60
}

fn default_temperature() -> f64 {
    0.2
}

fn default_retry_attempts() -> u32 {
    3
}

fn default_failure_cooldown() -> u64 {
    60
}

pub struct LlmAgent {
    client: reqwest::Client,
    base_url: String,
    model: String,
    api_key: Option<String>,
    symbols: Vec<String>,
    interval: String,
    min_interval: Duration,
    temperature: f64,
    retry_attempts: u32,
    failure_cooldown: Duration,
    next_run: Option<Instant>,
}

impl LlmAgent {
    fn from_config(config: SourceConfig) -> Result<Self> {
        let params: AgentParams = config
            .params
            .clone()
            .try_into()
            .map_err(|e| EngineError::Config(format!("llm_agent params: {e}")))?;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(params.timeout_secs))
            .build()
            .map_err(|e| EngineError::Other(format!("failed to build LLM HTTP client: {e}")))?;

        Ok(Self {
            client,
            base_url: params.base_url,
            model: params.model,
            api_key: params.api_key,
            symbols: config.symbols,
            interval: config.interval,
            min_interval: Duration::from_secs(params.min_interval_secs),
            temperature: params.temperature,
            retry_attempts: params.retry_attempts.max(1),
            failure_cooldown: Duration::from_secs(params.failure_cooldown_secs.max(1)),
            next_run: None,
        })
    }

    async fn ask(&self, ctx: &TickContext) -> Result<Vec<SignalIntent>> {
        let prompt = self.build_prompt(ctx);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let body = ChatRequest {
            model: &self.model,
            messages: vec![
                Message {
                    role: "system",
                    content:
                        "You are a conservative equity trading analyst. You only output valid JSON."
                            .to_string(),
                },
                Message {
                    role: "user",
                    content: prompt,
                },
            ],
            temperature: self.temperature,
            response_format: ResponseFormat {
                kind: "json_object",
            },
        };

        let mut request = self.client.post(&url).json(&body);
        if let Some(api_key) = &self.api_key {
            request = request.bearer_auth(api_key);
        }

        let response = request
            .send()
            .await
            .map_err(|e| EngineError::Other(format!("LLM request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(EngineError::Other(format!(
                "LLM returned HTTP {status}: {text}"
            )));
        }

        let payload: ChatResponse = response
            .json()
            .await
            .map_err(|e| EngineError::Other(format!("invalid LLM response: {e}")))?;
        let content = payload
            .choices
            .first()
            .and_then(|choice| choice.message.content.as_deref())
            .ok_or_else(|| EngineError::Other("LLM returned no content".to_string()))?;
        tracing::debug!(content, "LLM agent response");

        self.parse_signals(content)
    }

    fn build_prompt(&self, ctx: &TickContext) -> String {
        let mut market = String::new();
        for symbol in &self.symbols {
            if let Some(candles) = ctx.candles_for(symbol, &self.interval) {
                let start = candles.len().saturating_sub(30);
                let closes: Vec<f64> = candles[start..].iter().map(|c| c.close).collect();
                market.push_str(&format!(
                    "{symbol} ({interval}) last {count} closes: {closes:?}\n",
                    interval = self.interval,
                    count = closes.len()
                ));
            } else {
                market.push_str(&format!("{symbol}: no data available\n"));
            }
        }

        let social = if ctx.social.is_empty() {
            "none".to_string()
        } else {
            ctx.social
                .iter()
                .map(|post| {
                    format!(
                        "- [{}] {}: {}",
                        post.timestamp.format("%Y-%m-%d %H:%M"),
                        post.author,
                        post.text
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        format!(
            "Analyse the following equities for a paper-trading bot.\n\
             Only consider these symbols: {symbols}.\n\n\
             Recent market data:\n{market}\n\
             Recent social posts:\n{social}\n\n\
             Be conservative. Return ONLY JSON with this shape:\n\
             {{\"signals\":[{{\"symbol\":\"KO\",\"direction\":\"buy\"|\"sell\",\"confidence\":0.0,\"reason\":\"short explanation\",\"quantity_hint\":null}}]}}\n\
             If there is no actionable view, return {{\"signals\":[]}}.",
            symbols = self.symbols.join(", "),
            market = market,
            social = social
        )
    }

    fn parse_signals(&self, content: &str) -> Result<Vec<SignalIntent>> {
        let json = extract_json(content);
        let output: AgentOutput = serde_json::from_str(json)
            .map_err(|e| EngineError::Other(format!("could not parse LLM JSON: {e}")))?;

        let mut intents = Vec::new();
        for signal in output.signals {
            let symbol = match self
                .symbols
                .iter()
                .find(|symbol| symbol.eq_ignore_ascii_case(&signal.symbol))
            {
                Some(symbol) => symbol.clone(),
                None => {
                    tracing::warn!(
                        symbol = signal.symbol,
                        "LLM suggested a symbol outside the configured universe; ignoring"
                    );
                    continue;
                }
            };

            let direction = match signal.direction.to_ascii_lowercase().as_str() {
                "buy" | "long" => Side::Buy,
                "sell" | "short" => Side::Sell,
                other => {
                    tracing::warn!(
                        direction = other,
                        "LLM returned unknown direction; ignoring"
                    );
                    continue;
                }
            };

            let reason = if signal.reason.trim().is_empty() {
                "LLM signal".to_string()
            } else {
                signal.reason
            };
            let mut intent =
                SignalIntent::new(symbol, direction, signal.confidence, reason, SOURCE_NAME);

            if let Some(hint) = signal.quantity_hint {
                if hint > 0.0 {
                    if let Ok(quantity) = decimal_from_f64(hint) {
                        intent.quantity_hint = Some(quantity);
                    }
                }
            }

            intents.push(intent);
        }

        Ok(intents)
    }
}

#[async_trait]
impl SignalSource for LlmAgent {
    fn name(&self) -> &str {
        SOURCE_NAME
    }

    async fn on_tick(&mut self, ctx: &TickContext) -> Vec<SignalIntent> {
        let now = Instant::now();
        if let Some(next_run) = self.next_run {
            if now < next_run {
                return Vec::new();
            }
        }

        let mut last_error = None;
        for attempt in 1..=self.retry_attempts {
            match self.ask(ctx).await {
                Ok(intents) => {
                    self.next_run = Some(Instant::now() + self.min_interval);
                    tracing::info!(signals = intents.len(), "LLM agent completed");
                    return intents;
                }
                Err(err) => {
                    tracing::warn!(
                        attempt,
                        attempts = self.retry_attempts,
                        error = %err,
                        "LLM agent attempt failed"
                    );
                    last_error = Some(err);
                    if attempt < self.retry_attempts {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        }

        if let Some(err) = last_error {
            tracing::warn!(
                error = %err,
                cooldown_secs = self.failure_cooldown.as_secs(),
                "LLM agent unavailable; backing off"
            );
        }
        self.next_run = Some(Instant::now() + self.failure_cooldown);
        Vec::new()
    }
}

fn extract_json(content: &str) -> &str {
    let trimmed = content.trim();
    let start = trimmed.find('{').unwrap_or(0);
    let end = trimmed
        .rfind('}')
        .map(|index| index + 1)
        .unwrap_or(trimmed.len());
    &trimmed[start..end]
}

pub fn register(registry: &mut SourceRegistry) {
    registry.register(SOURCE_NAME, |config| {
        LlmAgent::from_config(config).map(|agent| Box::new(agent) as Box<dyn SignalSource>)
    });
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<Message>,
    temperature: f64,
    response_format: ResponseFormat,
}

#[derive(Serialize)]
struct Message {
    role: &'static str,
    content: String,
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

#[derive(Deserialize)]
struct AgentOutput {
    #[serde(default)]
    signals: Vec<AgentSignal>,
}

#[derive(Deserialize)]
struct AgentSignal {
    symbol: String,
    direction: String,
    #[serde(default = "default_confidence")]
    confidence: f64,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    quantity_hint: Option<f64>,
}

fn default_confidence() -> f64 {
    0.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    fn agent() -> LlmAgent {
        LlmAgent {
            client: reqwest::Client::new(),
            base_url: "http://localhost:8787/v1".to_string(),
            model: "deepseek-v4.1-flash".to_string(),
            api_key: None,
            symbols: vec!["KO".to_string(), "PEP".to_string()],
            interval: "1d".to_string(),
            min_interval: Duration::from_secs(3_600),
            temperature: 0.2,
            retry_attempts: 3,
            failure_cooldown: Duration::from_secs(60),
            next_run: None,
        }
    }

    #[test]
    fn parses_signals_and_filters_the_universe() {
        let content = r#"{"signals":[
            {"symbol":"ko","direction":"buy","confidence":0.8,"reason":"test","quantity_hint":2.5},
            {"symbol":"TSLA","direction":"sell","confidence":0.9,"reason":"not in universe"}
        ]}"#;

        let intents = agent().parse_signals(content).unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].symbol, "KO");
        assert_eq!(intents[0].direction, Side::Buy);
        assert_eq!(intents[0].quantity_hint, Some(Decimal::new(25, 1)));
    }

    #[test]
    fn parses_fenced_json() {
        let content = "Here is the result:\n```json\n{\"signals\":[]}\n```\n";
        let intents = agent().parse_signals(content).unwrap();
        assert!(intents.is_empty());
    }

    #[test]
    fn rejects_unknown_directions() {
        let content = r#"{"signals":[{"symbol":"KO","direction":"sideways","confidence":0.5,"reason":"test"}]}"#;
        assert!(agent().parse_signals(content).unwrap().is_empty());
    }
}
