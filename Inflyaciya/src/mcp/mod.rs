pub mod auth;
pub mod client;
pub mod models;
pub mod products;

use serde_json::{Value, json};
use std::{env, fs};

use self::{
    auth::AuthStatus,
    client::McpClient,
    products::{collect_sample, compare_search_results, search_arguments, unwrap_tool_data},
};

pub const DEFAULT_ENDPOINT: &str = "https://mcp.vkusvill.ru/mcp";

pub fn print_help() {
    println!(
        "Inflyaciya research CLI\n\n\
Usage:\n  inflyaciya-phase0 [sample.json]\n  inflyaciya-phase0 --phase05 [sample.json]\n  inflyaciya-phase0 mcp <command> [arguments]\n\n\
MCP commands:\n  ping                         initialize and show server capabilities\n  tools                        list tool names and input schemas\n  search <query> [limit]       public product search (1..10 items)\n  details <product-id>         public product details\n  barcode <ean13>              public lookup by EAN-13\n  discount [page]              public discount listing\n  shops [page]                 public shop directory page\n  repeat <query>               repeat the same search and compare it\n  sample [output.json]         collect up to 30 public observations\n  auth-status                  inspect OAuth metadata without logging in\n\n\
Environment:\n  VUSVILL_MCP_ENDPOINT         override the endpoint\n  VUSVILL_MCP_ACCESS_TOKEN     optional existing OAuth access token\n\n\
Safety: cart creation, order history and other personal tools are blocked."
    );
}

fn endpoint() -> String {
    env::var("VUSVILL_MCP_ENDPOINT").unwrap_or_else(|_| DEFAULT_ENDPOINT.to_owned())
}

fn initialized_client() -> Result<(McpClient, Value), String> {
    let mut client = McpClient::new(&endpoint())?;
    let initialized = client.initialize()?;
    client.initialized()?;
    Ok((client, initialized))
}

fn pretty(value: &Value) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())?
    );
    Ok(())
}

pub fn run_cli(args: &[String]) -> Result<(), String> {
    let Some(command) = args.first().map(String::as_str) else {
        print_help();
        return Ok(());
    };
    if matches!(command, "help" | "--help" | "-h") {
        print_help();
        return Ok(());
    }
    if command == "auth-status" {
        let client = reqwest::blocking::Client::new();
        return pretty(
            &serde_json::to_value(AuthStatus::discover(&client)?)
                .map_err(|error| error.to_string())?,
        );
    }

    let (mut client, initialized) = initialized_client()?;
    let result = match command {
        "ping" => pretty(&initialized),
        "tools" => pretty(
            &serde_json::to_value(client.tool_definitions()?).map_err(|error| error.to_string())?,
        ),
        "search" => {
            let query = args.get(1).ok_or("search requires a query")?;
            let limit = args
                .get(2)
                .map(|raw| raw.parse::<u8>().map_err(|error| error.to_string()))
                .transpose()?
                .unwrap_or(10);
            let response = client
                .call_read_only_tool("vkusvill_products_search", search_arguments(query, limit)?)?;
            pretty(&unwrap_tool_data(&response)?)
        }
        "details" => {
            let id = args
                .get(1)
                .ok_or("details requires a product id")?
                .parse::<u64>()
                .map_err(|error| error.to_string())?;
            let response =
                client.call_read_only_tool("vkusvill_product_details", json!({ "id": id }))?;
            pretty(&unwrap_tool_data(&response)?)
        }
        "barcode" => {
            let barcode = args.get(1).ok_or("barcode requires an EAN-13 value")?;
            if barcode.len() != 13 || !barcode.chars().all(|character| character.is_ascii_digit()) {
                return Err("barcode must contain exactly 13 digits".to_owned());
            }
            let response = client
                .call_read_only_tool("vkusvill_product_barcode", json!({ "barcode": barcode }))?;
            pretty(&unwrap_tool_data(&response)?)
        }
        "discount" => {
            let page = args
                .get(1)
                .map(|raw| raw.parse::<u64>().map_err(|error| error.to_string()))
                .transpose()?
                .unwrap_or(1);
            let response = client.call_read_only_tool(
                "vkusvill_products_discount",
                json!({ "type": "card", "page": page, "sort": "popularity", "vvonly": 0 }),
            )?;
            pretty(&unwrap_tool_data(&response)?)
        }
        "shops" => {
            let page = args
                .get(1)
                .map(|raw| raw.parse::<u64>().map_err(|error| error.to_string()))
                .transpose()?
                .unwrap_or(1);
            let response = client.call_read_only_tool("vkusvill_shops", json!({ "page": page }))?;
            pretty(&unwrap_tool_data(&response)?)
        }
        "repeat" => {
            let query = args.get(1).ok_or("repeat requires a query")?;
            let first = unwrap_tool_data(
                &client
                    .call_read_only_tool("vkusvill_products_search", search_arguments(query, 5)?)?,
            )?;
            let second = unwrap_tool_data(
                &client
                    .call_read_only_tool("vkusvill_products_search", search_arguments(query, 5)?)?,
            )?;
            pretty(&compare_search_results(query, &first, &second))
        }
        "sample" => {
            let output_path = args
                .get(1)
                .map(String::as_str)
                .unwrap_or("research/phase06_sample_products.json");
            let sample = collect_sample(&mut client)?;
            let serialized =
                serde_json::to_string_pretty(&sample).map_err(|error| error.to_string())?;
            fs::write(output_path, format!("{serialized}\n"))
                .map_err(|error| format!("cannot write {output_path}: {error}"))?;
            println!(
                "wrote {} observations to {output_path}",
                sample.observations.len()
            );
            Ok(())
        }
        other => Err(format!("unknown MCP command {other:?}; use --help")),
    };
    let close_result = client.close();
    result.and(close_result)
}
