use regex::Regex;
use reqwest::{
    header::{HeaderMap, HeaderValue, AUTHORIZATION},
    Client,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;

#[derive(Serialize, Deserialize)]
struct Config {
    auth_email: String,
    auth_method: String,
    auth_key: String,
    zone_identifier: String,
    record_name: String,
    ttl: u32,
    proxy: bool,
}

#[derive(Serialize, Deserialize)]
struct Payload {
    r#type: String,
    name: String,
    content: String,
    ttl: u32,
    proxied: bool,
}

const IPV4_REGEX: &str = r#"([01]?[0-9]?[0-9]|2[0-4][0-9]|25[0-5])\.([01]?[0-9]?[0-9]|2[0-4][0-9]|25[0-5])\.([01]?[0-9]?[0-9]|2[0-4][0-9]|25[0-5])\.([01]?[0-9]?[0-9]|2[0-4][0-9]|25[0-5])"#;
const CLOUDFLARE_URL: &str = "https://cloudflare.com/cdn-cgi/trace";
const IPIFY_URL: &str = "https://api.ipify.org";
const ICANHAZIP_URL: &str = "https://ipv4.icanhazip.com";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config_path = format!(
        "{}/.config/ddns/config.json",
        std::env::var("HOME").unwrap()
    );
    let config_contents = fs::read_to_string(&config_path).expect(&format!(
        "Could not read the config file.\nconfig: {}",
        config_path
    ));
    let config: Config = serde_json::from_str(&config_contents)
        .expect(&format!("Invalid config file.\nconfig: {}", config_path));

    // Get the ip from Cloudflare
    let ip = match reqwest::get(CLOUDFLARE_URL).await {
        Ok(response) => {
            let body = response.text().await?;
            match body.lines().find(|line| line.starts_with("ip=")) {
                Some(line) => {
                    let re = Regex::new(IPV4_REGEX)?;
                    let ip = re
                        .captures(line)
                        .ok_or_else(|| "failed to extract IP from Cloudflare response")?
                        .get(0)
                        .map_or("", |m| m.as_str());
                    String::from(ip)
                }
                None => {
                    // Attempt to get the ip from other websites.
                    let ip = match reqwest::get(IPIFY_URL).await {
                        Ok(response) => response.text().await?,
                        Err(_) => reqwest::get(ICANHAZIP_URL).await?.text().await?,
                    };
                    String::from(ip.trim())
                }
            }
        }
        Err(_) => {
            // Attempt to get the ip from other websites.
            let ip = match reqwest::get(IPIFY_URL).await {
                Ok(response) => response.text().await?,
                Err(_) => reqwest::get(ICANHAZIP_URL).await?.text().await?,
            };
            String::from(ip.trim())
        }
    };

    // Use regex to check for proper IPv4 format.
    if !Regex::new(&format!("^{}$", IPV4_REGEX))?.is_match(&ip) {
        eprintln!("ddns updater: Failed to find a valid IP.");
        std::process::exit(2);
    }

    let client = Client::new();

    // Build the request headers
    let mut headers = HeaderMap::new();
    headers.insert(
        "X-Auth-Email",
        HeaderValue::from_str(&config.auth_email).expect("Invalid email address"),
    );
    match config.auth_method.as_str() {
        "global" => {
            headers.insert(
                "X-Auth-Key",
                HeaderValue::from_str(&config.auth_key).expect("Invalid auth key"),
            );
        }
        "token" => {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", &config.auth_key))
                    .expect("Invalid auth key"),
            );
        }
        _ => {
            println!("The authentication method should either be global or token.\nExpected: \n....\n\"auth_method\": \"global\" or \"token\",\n....");
            println!(
                "\nFound: \n....\n\"auth_method\": {},\n....",
                config.auth_method
            );
            std::process::exit(1);
        }
    }
    headers.insert("Content-Type", HeaderValue::from_static("application/json"));

    // Build the GET request and execute it
    let response = client
        .get(format!(
            "https://api.cloudflare.com/client/v4/zones/{}/dns_records?type=A&name={}",
            config.zone_identifier, config.record_name
        ))
        .headers(headers.clone())
        .send()
        .await
        .expect("Failed to send request");

    // Read the response body
    let record = response.text().await.expect("Failed to read response body");

    if record.contains("\"count\":0") {
        eprintln!(
            "ddns updater: Record does not exist, perhaps create one first? ({} for {})",
            &ip, &config.record_name
        );
        std::process::exit(1);
    }

    let json: Value = serde_json::from_str(&record)?;

    if let Some(content) = json["result"][0]["content"].as_str() {
        let current_ip = content.to_owned();

        if ip == current_ip {
            println!(
                "ddns updater: IP ({}) for {} has not changed.",
                ip, &config.record_name
            );
            std::process::exit(0);
        }
    }

    let record_identifier = if let Some(content) = json["result"][0]["id"].as_str() {
        content.to_owned()
    } else {
        return Err("Error: could not extract content from JSON response".into());
    };

    let data = json!({
            "type": "A",
            "name": config.record_name,
            "content": ip,
            "ttl": config.ttl,
            "proxied": config.proxy
    });

    let payload: Payload = serde_json::from_value(data).unwrap();

    let response = client
        .patch(format!(
            "https://api.cloudflare.com/client/v4/zones/{}/dns_records/{}",
            &config.zone_identifier, record_identifier
        ))
        .headers(headers)
        .json(&payload)
        .send()
        .await
        .expect("Failed to read response body")
        .text()
        .await?;

    let json: Value = serde_json::from_str(&response)?;

    if json["success"].as_bool().unwrap_or(false) {
        println!("DNS updated.");
    } else {
        eprintln!("Error: HTTP response: \n{:#?}", json);
        return Ok(());
    };

    Ok(())
}
