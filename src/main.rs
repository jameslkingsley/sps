use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use anyhow::Result;
use clap::{Parser, Subcommand};
use http::{HeaderMap, HeaderValue, header::AUTHORIZATION};
use reqwest::Client;
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{RetryTransientMiddleware, policies::ExponentialBackoff};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Parser)]
struct Args {
    #[clap(subcommand)]
    command: Command,

    #[arg(long, default_value_t = false)]
    dry_run: bool,

    #[clap(env)]
    square_location_id: String,

    #[clap(env)]
    square_app_id: String,

    #[clap(env)]
    square_access_token: String,
}

#[derive(Debug, Clone, Subcommand)]
enum Command {
    DeleteDuplicateGtins,
}

#[tokio::main]
pub async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let args = Args::parse();
    let client = square_client(&args);

    match args.command {
        Command::DeleteDuplicateGtins => {
            let variations = get_item_variations(&client).await?;
            let mut by_upc: HashMap<String, Vec<ItemVariation>> = HashMap::default();

            for variation in variations {
                let Some(upc) = variation.data.upc.clone() else {
                    continue;
                };

                by_upc.entry(upc).or_default().push(variation);
            }

            by_upc.retain(|_, vec| vec.len() > 1);

            let mut duplicate_variations: HashSet<String> = HashSet::default();

            for (_barcode, mut variations) in by_upc.into_iter() {
                variations.sort_unstable_by_key(|v| v.version);

                if let None = variations.pop() {
                    continue;
                }

                if !variations.is_empty() {
                    duplicate_variations.extend(variations.iter().map(|v| v.id.clone()));
                }
            }

            if !duplicate_variations.is_empty() {
                let duplicate_variations = duplicate_variations.into_iter().collect::<Vec<_>>();

                for chunk in duplicate_variations.chunks(200) {
                    if args.dry_run {
                        println!("[DRY RUN] Would delete {} objects in chunk", chunk.len());
                    } else {
                        println!("Deleting {} objects in chunk..", chunk.len());
                        client
                            .post("https://connect.squareup.com/v2/catalog/batch-delete")
                            .json(&json!({
                                "object_ids": chunk
                            }))
                            .send()
                            .await?
                            .error_for_status()?;
                    }
                }

                println!("Done");
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ItemVariation {
    id: String,
    version: i64,
    is_deleted: bool,
    #[serde(rename = "item_variation_data")]
    data: ItemVariationData,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ItemVariationData {
    name: String,
    item_id: String,
    sku: Option<String>,
    upc: Option<String>,
    pricing_type: String,
    price_money: Option<Money>,
    default_unit_cost: Option<Money>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Money {
    amount: i64,
    currency: String,
}

async fn get_item_variations(client: &ClientWithMiddleware) -> Result<Vec<ItemVariation>> {
    let mut cursor = Some(String::new());
    let mut result = Vec::new();

    while let Some(current_cursor) = cursor.as_deref() {
        let mut query = vec![("types", "ITEM_VARIATION")];
        if !current_cursor.is_empty() {
            query.push(("cursor", current_cursor));
        }

        match client
            .get("https://connect.squareup.com/v2/catalog/list")
            .query(&query)
            .send()
            .await?
            .error_for_status()
        {
            Ok(res) => {
                let mut data: Value = res.json().await?;
                cursor = data.get("cursor").and_then(|c| c.as_str().map(Into::into));
                result.extend(match data.get_mut("objects") {
                    Some(value) => serde_json::from_value(value.take())?,
                    None => Vec::new(),
                });
            }
            Err(err) => {
                eprintln!("{err}");
            }
        };
    }

    Ok(result)
}

fn square_client(args: &Args) -> ClientWithMiddleware {
    let mut headers = HeaderMap::new();

    let mut auth_value =
        HeaderValue::from_str(&format!("Bearer {}", args.square_access_token)).unwrap();
    auth_value.set_sensitive(true);
    headers.insert(AUTHORIZATION, auth_value);

    headers.insert("Square-Version", "2025-10-16".parse().unwrap());
    headers.insert("Content-Type", "application/json".parse().unwrap());

    let retry_policy = ExponentialBackoff::builder().build_with_max_retries(3);

    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .default_headers(headers)
        .build()
        .unwrap();

    ClientBuilder::new(client)
        .with(RetryTransientMiddleware::new_with_policy(retry_policy))
        .build()
}
