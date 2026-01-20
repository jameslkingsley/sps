use std::{collections::HashMap, time::Duration};

use anyhow::Result;
use clap::{Parser, Subcommand};
use http::{HeaderMap, HeaderValue, header::AUTHORIZATION};
use reqwest::Client;
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{RetryTransientMiddleware, policies::ExponentialBackoff};
use rust_decimal::{Decimal, RoundingStrategy, dec, prelude::ToPrimitive};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::task::JoinSet;

#[derive(Debug, Parser)]
struct Args {
    #[clap(subcommand)]
    command: Command,

    #[clap(env)]
    square_location_id: String,

    #[clap(env)]
    square_app_id: String,

    #[clap(env)]
    square_access_token: String,
}

#[derive(Debug, Clone, Subcommand)]
enum Command {
    ListZeroMargin,
    ApplyPriceTargets,
}

#[tokio::main]
pub async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let args = Args::parse();
    let client = square_client(&args);

    match args.command {
        Command::ListZeroMargin => {
            let rows = get_item_variations(&client)
                .await?
                .into_iter()
                .filter(|v| !v.is_deleted)
                .filter_map(|v| {
                    let unit = v.data.default_unit_cost.as_ref()?.amount;
                    let price = v.data.price_money.as_ref()?.amount;

                    if price > unit {
                        return None;
                    }

                    Some(format!(
                        "{},{},{},{},{}",
                        v.data.item_id, v.id, v.data.name, unit, price
                    ))
                })
                .collect::<Vec<_>>()
                .join("\n");
            println!("ItemId,VariationId,Name,UnitPrice,RetailPrice");
            println!("{}", rows);
        }
        Command::ApplyPriceTargets => {
            println!("Fetching catalog...");
            let variations = get_item_variations(&client)
                .await?
                .into_iter()
                .filter(|v| !v.is_deleted)
                .filter(|v| {
                    v.data
                        .default_unit_cost
                        .as_ref()
                        .is_some_and(|n| n.amount > 0)
                })
                .filter(|v| {
                    v.data.price_money.as_ref().is_some_and(|n| {
                        n.amount > 0
                            && n.amount != v.data.default_unit_cost.as_ref().unwrap().amount
                    })
                })
                .collect::<Vec<_>>();
            println!("Processing {} variations", variations.len());

            let item_tax_map = variations
                .chunks(1000)
                .map(|chunk| get_item_taxes(client.clone(), chunk.to_vec()))
                .collect::<JoinSet<_>>()
                .join_all()
                .await
                .into_iter()
                .flatten()
                .reduce(|mut acc, val| {
                    acc.extend(val);
                    acc
                })
                .unwrap();

            let updates = variations
                .iter()
                .filter_map(|v| {
                    let mut price =
                        v.price_data(match item_tax_map.get(&v.data.item_id)?.as_str() {
                            "2CJE55HCPJY5LHB4ZUEDCRJF" => dec!(0.0),
                            "3TXZ4AJ4DUCSI6YDBKXHBLRQ" => dec!(0.05),
                            "QDKOK36EMFC7772L2V64U6YD" => dec!(0.20),
                            _ => unreachable!(),
                        })?;

                    if price.por() >= dec!(0.4) {
                        return None;
                    }

                    let original = price.clone();
                    price.set_por(dec!(0.4));
                    price.round_to_retail();

                    Some((
                        original.por(),
                        price.clone().por(),
                        UpdateItemVariation {
                            kind: "ITEM_VARIATION".to_string(),
                            id: v.id.clone(),
                            data: UpdateItemVariationData {
                                price_money: Money {
                                    amount: (price.rrp * dec!(100)).round_dp(2).to_i64()?,
                                    currency: "GBP".to_string(),
                                },
                            },
                        },
                    ))
                })
                .collect::<Vec<_>>();

            dbg!(&updates);
            println!("Updating {} prices", updates.len());
            // curl https://connect.squareup.com/v2/catalog/list?types=ITEM_VARIATION \
            //   -H 'Square-Version: 2025-10-16' \
            //   -H 'Authorization: Bearer' \
            //   -H 'Content-Type: application/json'
            // -----------------------------------------------------------------------------------------------
            // curl https://connect.squareup.com/v2/catalog/batch-upsert \
            //   -X POST \
            //   -H 'Square-Version: 2025-10-16' \
            //   -H 'Authorization: Bearer' \
            //   -H 'Content-Type: application/json' \
            //   -d '{
            //     "batches": [
            //       {
            //         "objects": [
            //           {
            //             "type": "ITEM_VARIATION",
            //             "item_variation_data": {
            //               "price_money": {
            //                 "amount": 123,
            //                 "currency": "GBP"
            //               }
            //             },
            //             "id": ""
            //           }
            //         ]
            //       }
            //     ],
            //     "idempotency_key": "bdf1cfff-aaf7-4b73-82d3-39068a71fcb9"
            //   }'
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct UpdateItemVariation {
    #[serde(rename = "type")]
    kind: String,
    id: String,
    #[serde(rename = "item_variation_data")]
    data: UpdateItemVariationData,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct UpdateItemVariationData {
    price_money: Money,
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
    pricing_type: String,
    price_money: Option<Money>,
    default_unit_cost: Option<Money>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Money {
    amount: i64,
    currency: String,
}

#[derive(Debug, Clone)]
struct PriceData {
    rrp: Decimal,
    unit: Decimal,
    tax_rate: Decimal,
}

impl ItemVariation {
    pub fn price_data(&self, tax_rate: Decimal) -> Option<PriceData> {
        let rrp =
            (Decimal::from(self.data.price_money.as_ref()?.amount) / dec!(100)).trunc_with_scale(2);

        let unit = (Decimal::from(self.data.default_unit_cost.as_ref()?.amount) / dec!(100))
            .trunc_with_scale(2);

        Some(PriceData {
            rrp,
            unit,
            tax_rate,
        })
    }
}

impl PriceData {
    pub fn vat(&self) -> Decimal {
        (self.rrp - (self.rrp / (self.tax_rate + dec!(1)))).round_dp(2)
    }

    pub fn net(&self) -> Decimal {
        (self.rrp - self.vat()).round_dp(2)
    }

    pub fn profit(&self) -> Decimal {
        self.net() - self.unit
    }

    pub fn por(&self) -> Decimal {
        let net = self.net();
        let profit = self.profit();
        (profit / net).round_dp(2)
    }

    pub fn set_por(&mut self, por: Decimal) {
        let net = self.unit / (dec!(1) - por);
        let gross =
            net.round_dp_with_strategy(2, RoundingStrategy::ToZero) * (self.tax_rate + dec!(1));
        self.rrp = gross.round_dp_with_strategy(2, RoundingStrategy::ToZero);
    }

    pub fn round_to_retail(&mut self) {
        let pennies = (self.rrp * dec!(100)).round_dp(0);
        let Some(mut pennies) = pennies.to_i64() else {
            return;
        };

        let sign = if pennies < 0 { -1 } else { 1 };
        let last_digit = (pennies.abs() % 10) as i64;
        let target = if last_digit <= 2 {
            0
        } else if last_digit <= 5 {
            5
        } else {
            9
        };
        pennies += sign * (target - last_digit);

        self.rrp = Decimal::new(pennies, 2);
    }
}

async fn get_item_taxes(
    client: ClientWithMiddleware,
    variations: Vec<ItemVariation>,
) -> Result<HashMap<String, String>> {
    Ok(client
        .post("https://connect.squareup.com/v2/catalog/batch-retrieve")
        .json(&json!({
            "object_ids": variations.iter().map(|v| v.data.item_id.clone()).collect::<Vec<_>>(),
            "include_category_path_to_root": false,
            "include_related_objects": false
        }))
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?
        .get("objects")
        .and_then(|objects| {
            Some(
                objects
                    .as_array()
                    .expect("as_array")
                    .into_iter()
                    .filter_map(|obj| {
                        Some((
                            obj.get("id").expect("id").as_str()?.to_string(),
                            obj.get("item_data")
                                .expect("item_data")
                                .get("tax_ids")?
                                .as_array()
                                .expect("as_array")
                                .get(0)
                                .expect("get(0)")
                                .as_str()?
                                .to_string(),
                        ))
                    })
                    .collect::<HashMap<_, _>>(),
            )
        })
        .unwrap_or_default())
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

// {
//   "cursor": "CAASIQoSNzU5MzQ1NjQ6MjIxMDg0NTQ1EgQQBThkGJThtru9Mw",
//   "objects": [
//     {
//       "type": "ITEM_VARIATION",
//       "id": "G7UJHOOKEF5SM7BXDS7EIHHJ",
//       "updated_at": "2025-09-29T19:11:49.983Z",
//       "created_at": "2025-08-03T20:06:36.982Z",
//       "version": 1759173109983,
//       "is_deleted": false,
//       "present_at_all_locations": false,
//       "present_at_location_ids": [
//         "LMN05M0VXFMN4"
//       ],
//       "item_variation_data": {
//         "item_id": "62OGYRY7KDF6FDOZK5ZL3SR6",
//         "name": "",
//         "sku": "SLE-169072",
//         "upc": "7622202209819",
//         "ordinal": 1,
//         "pricing_type": "FIXED_PRICING",
//         "price_money": {
//           "amount": 135,
//           "currency": "GBP"
//         },
//         "location_overrides": [
//           {
//             "location_id": "LMN05M0VXFMN4",
//             "track_inventory": true,
//             "inventory_alert_type": "LOW_QUANTITY",
//             "inventory_alert_threshold": 5,
//             "sold_out": true
//           }
//         ],
//         "track_inventory": true,
//         "sellable": true,
//         "stockable": true,
//         "default_unit_cost": {
//           "amount": 86,
//           "currency": "GBP"
//         },
//         "channels": [
//           "CH_PVGq6yxbkGGZiMB1SwK7qwfpE3I7hBM8sbc6vTR29945o"
//         ],
//         "item_variation_vendor_info_ids": [
//           "P2BLP2Q7SYV27CHSDQX4HWGV"
//         ],
//         "item_variation_vendor_infos": [
//           {
//             "type": "ITEM_VARIATION_VENDOR_INFO",
//             "id": "P2BLP2Q7SYV27CHSDQX4HWGV",
//             "updated_at": "2025-08-03T20:06:35.535Z",
//             "created_at": "2025-08-03T20:06:36.982Z",
//             "version": 1754251595535,
//             "is_deleted": false,
//             "present_at_all_locations": false,
//             "present_at_location_ids": [
//               "LMN05M0VXFMN4"
//             ],
//             "item_variation_vendor_info_data": {
//               "ordinal": 1,
//               "price_money": {
//                 "amount": 86,
//                 "currency": "GBP"
//               },
//               "item_variation_id": "G7UJHOOKEF5SM7BXDS7EIHHJ",
//               "vendor_id": "OQK27WIYLL5QWXE3"
//             }
//           }
//         ]
//       }
//     }
//   ]
// }
