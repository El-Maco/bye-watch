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
    currencies: Vec<CurrencyAlert>,
}

#[derive(Debug, Deserialize, Serialize)]
struct CurrencyAlert {
    symbol: String,
    threshold: f64,
    alert_condition: String,
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
        "Checking {} currencies every {} seconds",
        config.currencies.len(),
        config.check_interval
    );

    loop {
        match check_currencies(&mut config) {
            Ok(_) => println!("Check completed at {}", Local::now().format("%d-%m-%Y %H:%M:%S")),
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

    let mut filtered_prices: Vec<BinancePrice> = vec![];
    for price_data in prices {
        if currency_symbols.contains(&price_data.symbol) {
            filtered_prices.push(price_data);
        }
    }
    Ok(filtered_prices)
}

fn check_currencies(config: &mut Config) -> Result<(), Box<dyn std::error::Error>> {
    let prices = fetch_prices(config)?;

    let mut body = String::new();
    let current_time = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    for currency in &mut config.currencies {
        if let Some(current_price) = prices.iter().find(|p| p.symbol == currency.symbol) {
            let alert_triggered = match currency.alert_condition.as_str() {
                "above" => current_price.price.parse::<f64>().unwrap() > currency.threshold,
                "below" => current_price.price.parse::<f64>().unwrap() < currency.threshold,
                _ => false,
            };

            if alert_triggered {
                let should_alert = match currency.last_alerted {
                    Some(timestamp) => {
                        current_time - timestamp > 86400
                    },
                    None => true
                };
                if should_alert {
                    println!(
                        "Alert triggered for {}: price {} is {} threshold {}",
                        currency.symbol,
                        current_price.price,
                        currency.alert_condition,
                        currency.threshold
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
                    println!("Alert condition met for {}, but already alerted within 24 hours", currency.symbol);
                }
            } else {
                if currency.last_alerted.is_some() {
                    println!("Condition no longer met for {}, resetting alert status", currency.symbol);
                    currency.last_alerted = None;
                }
            }
        } else {
            eprintln!("No price data found for {}", currency.symbol);
        }
    }

    if !body.is_empty() {
        let body = format!("Found the following crypto alerts\n\n {}", body);
        // send_email(config, "[bye-watch] Price Alert", &body)?;
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
