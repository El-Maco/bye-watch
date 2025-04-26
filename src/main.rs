use chrono::Local;
use lettre::{transport::smtp::authentication::Credentials, Message, SmtpTransport, Transport};
use reqwest;
use serde::Deserialize;
use std::fs;
use std::time::Duration;

#[derive(Debug, Deserialize)]
struct EmailConfig {
    username: String,
    password: String,
}
#[derive(Debug, Deserialize)]
struct Config {
    email: EmailConfig,
    check_interval: u64,
    currencies: Vec<CurrencyAlert>,
}

#[derive(Debug, Deserialize)]
struct CurrencyAlert {
    symbol: String,
    threshold: f64,
    alert_condition: String,
}

#[derive(Debug, Deserialize)]
struct BinancePrice {
    symbol: String,
    price: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config_content = fs::read_to_string("config.json")?;
    let config: Config = serde_json::from_str(&config_content)?;

    println!("Bye-Watch Started");
    println!(
        "Checking {} currencies every {} seconds",
        config.currencies.len(),
        config.check_interval
    );

    loop {
        match check_currencies(&config) {
            Ok(_) => println!("Check completed at {}", Local::now().format("%d-%m-%Y %H:%M:%S")),
            Err(e) => eprintln!("Error during check: {}", e),
        }

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

fn check_currencies(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let prices = fetch_prices(config)?;

    let mut body = String::new();
    for currency in &config.currencies {
        if let Some(current_price) = prices.iter().find(|p| p.symbol == currency.symbol) {
            let alert = match currency.alert_condition.as_str() {
                "above" => current_price.price.parse::<f64>().unwrap() > currency.threshold,
                "below" => current_price.price.parse::<f64>().unwrap() < currency.threshold,
                _ => false,
            };

            if alert {
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
            }
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
