mod email;

use reqwest;
use serde::Deserialize;
use std::fs;

#[derive(Debug, Deserialize)]
struct Config {
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

    check_currencies(&config)
}

fn check_currencies(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::blocking::Client::new();

    for currency in &config.currencies {
        let url = format!(
            "https://api.binance.com/api/v3/ticker/price?symbol={}",
            currency.symbol
        );
        let response = client.get(&url).send()?;

        if !response.status().is_success() {
            eprintln!(
                "Failed to fetch price for {}: HTTP {}",
                currency.symbol,
                response.status()
            );
            continue;
        }

        let price_data: BinancePrice = response.json()?;
        let current_price: f64 = price_data.price.parse()?;

        let alert = match currency.alert_condition.as_str() {
            "above" => current_price > currency.threshold,
            "below" => current_price < currency.threshold,
            _ => false,
        };

        if alert {
            println!(
                "Alert triggered for {}: price {} is {} threshold {}",
                price_data.symbol, current_price, currency.alert_condition, currency.threshold
            );
        }
    }
    Ok(())
}
