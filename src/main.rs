use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use http::{HeaderMap, HeaderValue, header::AUTHORIZATION};
use reqwest::Client;
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{RetryTransientMiddleware, policies::ExponentialBackoff};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Parser)]
struct Args {
    #[clap(env)]
    square_location_id: String,

    #[clap(env)]
    square_app_id: String,

    #[clap(env)]
    square_access_token: String,
}

#[tokio::main]
pub async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let args = Args::parse();
    let client = square_client(&args);

    let variations = get_item_variations(&client).await?;

    dbg!(variations);

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

    Ok(())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ItemVariation {
    id: String,
    version: i64,
    is_deleted: bool,
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
