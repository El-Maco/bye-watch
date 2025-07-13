use chrono::Local;
use lettre::{transport::smtp::authentication::Credentials, Message, SmtpTransport, Transport};
use reqwest;
use serde::{Deserialize, Serialize};
use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Deserialize, Serialize)]
struct EmailConfig {
    username: String,
    password: String,
}
#[derive(Debug, Deserialize, Serialize)]
struct Config {
    email: EmailConfig,
    check_interval: u64,
    withold_notification_h: Option<u64>,
    currencies: Vec<CurrencyAlert>,
}

#[derive(Debug, Deserialize, Serialize)]
enum AlertCondition {
    Above,
    Below,
}

impl std::fmt::Display for AlertCondition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AlertCondition::Above => write!(f, "Above"),
            AlertCondition::Below => write!(f, "Below"),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct CurrencyAlert {
    symbol: String,
    threshold: f64,
    alert_condition: AlertCondition,
    last_alerted: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct BinancePrice {
    symbol: String,
    price: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config_path = "config.json";
    let config_content = fs::read_to_string(config_path)?;
    let mut config: Config = serde_json::from_str(&config_content)?;

    println!("Bye-Watch Started");
    println!(
        "Checking {} alerts every {} seconds",
        config.currencies.len(),
        config.check_interval
    );

    loop {
        match check_currencies(&mut config) {
            Ok(_) => println!(
                "Check completed at {}",
                Local::now().format("%d-%m-%Y %H:%M:%S")
            ),
            Err(e) => eprintln!("Error during check: {}", e),
        }

        let updated_config = serde_json::to_string_pretty(&config)?;
        fs::write(config_path, updated_config)?;
        std::thread::sleep(Duration::from_secs(config.check_interval));
    }
}

fn fetch_prices(config: &Config) -> Result<Vec<BinancePrice>, Box<dyn std::error::Error>> {
    let client = reqwest::blocking::Client::new();
    let url = "https://api.binance.com/api/v3/ticker/price";
    let response = client.get(url).send()?;
    if !response.status().is_success() {
        return Err(format!("Failed to fetch prices: HTTP {}", response.status()).into());
    }

    let prices: Vec<BinancePrice> = response.json()?;

    let currency_symbols: Vec<String> =
        config.currencies.iter().map(|c| c.symbol.clone()).collect();

    let filtered_prices: Vec<BinancePrice> = prices
        .into_iter()
        .filter(|price_data| currency_symbols.contains(&price_data.symbol))
        .collect();

    Ok(filtered_prices)
}

fn check_currencies(config: &mut Config) -> Result<(), Box<dyn std::error::Error>> {
    let prices = fetch_prices(config)?;

    let mut body = String::new();
    let current_time = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    for currency in &mut config.currencies {
        if let Some(current_price) = prices.iter().find(|p| p.symbol == currency.symbol) {
            let alert_triggered = match currency.alert_condition {
                AlertCondition::Above => {
                    current_price.price.parse::<f64>().unwrap() > currency.threshold
                }
                AlertCondition::Below => {
                    current_price.price.parse::<f64>().unwrap() < currency.threshold
                }
            };

            let withold_time_secs = config.withold_notification_h.unwrap_or(
                24 * 60 * 60, // Default to 24 hours if not specified
            );
            if alert_triggered {
                let should_alert = match currency.last_alerted {
                    Some(timestamp) => current_time - timestamp > withold_time_secs,
                    None => true,
                };
                if should_alert {
                    println!(
                        "Alert triggered for {} {} {}. Current price {}",
                        currency.symbol,
                        currency.alert_condition,
                        currency.threshold,
                        current_price.price,
                    );
                    let price_text = format!(
                        "\n{} {} threshold {}\nCurrent price: {:.2}\nTime: {}\n",
                        currency.symbol,
                        currency.alert_condition,
                        currency.threshold,
                        current_price.price.parse::<f64>().unwrap_or(0.0),
                        Local::now().format("%d-%m-%Y %H:%M:%S")
                    );
                    body.push_str(&price_text);
                    currency.last_alerted = Some(current_time);
                } else {
                    println!(
                        "Alert condition met for {} {} {}, but already alerted within {:.2} hours",
                        currency.symbol,
                        currency.alert_condition,
                        currency.threshold,
                        withold_time_secs as f64 / 3600.0,
                    );
                }
            } else {
                if currency.last_alerted.is_some() {
                    println!(
                        "Condition no longer met for {} {} {}, resetting alert status",
                        currency.symbol, currency.alert_condition, currency.threshold
                    );
                    currency.last_alerted = None;
                } else {
                    println!(
                        "Alert condition NOT met for {} {} {}, current price: {}",
                        currency.symbol,
                        currency.alert_condition,
                        currency.threshold,
                        current_price.price
                    );
                }
            }
        } else {
            eprintln!("No price data found for {}", currency.symbol);
        }
    }

    if !body.is_empty() {
        let body = format!("Found the following crypto alerts\n\n {}", body);
        send_email(config, "[bye-watch] Price Alert", &body)?;
        println!("{}", body);
    }

    Ok(())
}

fn send_email(
    config: &Config,
    subject: &str,
    body: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let email = Message::builder()
        .from(config.email.username.parse().unwrap())
        .to(config.email.username.parse().unwrap())
        .subject(subject)
        .body(body.to_string())
        .unwrap();

    let creds = Credentials::new(config.email.username.clone(), config.email.password.clone());
    let mailer = SmtpTransport::relay("smtp.gmail.com")
        .unwrap()
        .credentials(creds)
        .build();

    println!("Sending email");
    mailer.send(&email)?;
    Ok(())
}
