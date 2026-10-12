use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::{Value, json};
use std::str::FromStr;

use super::{DEFAULT_ENDPOINT, client::McpClient, models::ToolResult};
use crate::source::{
    CandidateObservation, GeographicBinding, SourceAdapter, Validation, validate_candidate,
};

#[derive(Debug, Serialize)]
pub struct SampleFile {
    pub generated_at: DateTime<Utc>,
    pub source: &'static str,
    pub protocol: &'static str,
    pub binding: &'static str,
    pub binding_reason: &'static str,
    pub observations: Vec<Value>,
    pub empty_category_checks: Vec<Value>,
}

struct VkusvillMcpSource;

impl SourceAdapter for VkusvillMcpSource {
    fn source_name(&self) -> &str {
        DEFAULT_ENDPOINT
    }

    fn candidate(&self, product_id: Option<String>) -> CandidateObservation {
        CandidateObservation {
            source: self.source_name().to_owned(),
            product_id,
            authorization_mode: "Anonymous".to_owned(),
            binding: GeographicBinding::Generic,
            store_id: None,
            store_address: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageMeasurement {
    pub amount: String,
    pub unit: String,
    pub normalized_quantity: String,
    pub normalized_unit: String,
    pub basis: String,
}

pub fn parse_package(name: &str) -> Option<PackageMeasurement> {
    let cleaned = name.replace("&nbsp;", " ").replace(',', ".");
    let tokens = cleaned.split_whitespace().collect::<Vec<_>>();
    for pair in tokens.windows(2).rev() {
        let raw_amount =
            pair[0].trim_matches(|character: char| !character.is_ascii_digit() && character != '.');
        let unit = pair[1].trim_matches(|character: char| !character.is_alphabetic());
        let multipack = pair[0]
            .split(['х', 'x', '×', '*'])
            .map(|part| {
                let number = part.trim_matches(|character: char| {
                    !character.is_ascii_digit() && character != '.'
                });
                Decimal::from_str(number).ok()
            })
            .collect::<Option<Vec<_>>>();
        let (amount, basis) = match multipack {
            Some(parts) if parts.len() == 2 => (parts[0] * parts[1], "product_name_multipack"),
            _ => match Decimal::from_str(raw_amount) {
                Ok(amount) => (amount, "product_name"),
                Err(_) => continue,
            },
        };
        if amount <= Decimal::ZERO {
            return None;
        }
        let (normalized_quantity, normalized_unit) = match unit {
            "г" => (amount / Decimal::from(1000u16), "kg"),
            "кг" => (amount, "kg"),
            "мл" => (amount / Decimal::from(1000u16), "l"),
            "л" => (amount, "l"),
            "шт" | "штук" => (amount, "piece"),
            _ => continue,
        };
        return Some(PackageMeasurement {
            amount: amount.normalize().to_string(),
            unit: unit.to_owned(),
            normalized_quantity: normalized_quantity.normalize().to_string(),
            normalized_unit: normalized_unit.to_owned(),
            basis: basis.to_owned(),
        });
    }
    None
}

pub fn normalize_public_item(item: &mut Value, query: &str, collected_at: DateTime<Utc>) {
    let adapter = VkusvillMcpSource;
    let candidate = adapter.candidate(item.get("id").map(Value::to_string));
    let validation = validate_candidate(candidate.clone());
    let mut package = item
        .get("name")
        .and_then(Value::as_str)
        .and_then(parse_package);
    if package.is_none() {
        package = item.get("weight").and_then(|weight| {
            let value = Decimal::from_str(&weight.get("value")?.to_string()).ok()?;
            let unit = weight.get("unit")?.as_str()?;
            if value <= Decimal::ZERO || unit != "кг" {
                return None;
            }
            Some(PackageMeasurement {
                amount: value.normalize().to_string(),
                unit: "кг".to_owned(),
                normalized_quantity: value.normalize().to_string(),
                normalized_unit: "kg".to_owned(),
                basis: "mcp_weight".to_owned(),
            })
        });
    }
    if package.is_none() && item.get("unit").and_then(Value::as_str) == Some("кг") {
        package = Some(PackageMeasurement {
            amount: "1".to_owned(),
            unit: "кг".to_owned(),
            normalized_quantity: "1".to_owned(),
            normalized_unit: "kg".to_owned(),
            basis: "mcp_unit_price".to_owned(),
        });
    }
    let normalized_price = package.as_ref().and_then(|measurement| {
        let price = Decimal::from_str(&item.get("price")?.get("current")?.to_string()).ok()?;
        let quantity = Decimal::from_str(&measurement.normalized_quantity).ok()?;
        if quantity <= Decimal::ZERO {
            return None;
        }
        Some(json!({
            "value": (price / quantity).round_dp(2).to_string(),
            "unit": match measurement.normalized_unit.as_str() {
                "kg" => "RUB/kg",
                "l" => "RUB/l",
                "piece" => "RUB/piece",
                _ => return None,
            }
        }))
    });
    if let Some(object) = item.as_object_mut() {
        object.insert("query".to_owned(), json!(query));
        object.insert("brand".to_owned(), Value::Null);
        object.insert("package".to_owned(), json!(package));
        object.insert(
            "normalized_price".to_owned(),
            normalized_price.unwrap_or(Value::Null),
        );
        object.insert("collected_at".to_owned(), json!(collected_at));
        object.insert(
            "authorization_mode".to_owned(),
            json!(candidate.authorization_mode),
        );
        object.insert("store_binding".to_owned(), json!(candidate.binding));
        object.insert("store_id".to_owned(), json!(candidate.store_id));
        object.insert("store_address".to_owned(), json!(candidate.store_address));
        object.insert("availability".to_owned(), Value::Null);
        object.insert("personalized".to_owned(), json!(false));
        object.insert("source".to_owned(), json!(candidate.source));
        object.insert("tool".to_owned(), json!("vkusvill_products_search"));
        object.insert(
            "validation".to_owned(),
            json!(match validation {
                Validation::CandidateOnly { reason } => json!({
                    "status": "CandidateObservation",
                    "reason": reason
                }),
                Validation::Confirmed(confirmed) => json!({
                    "status": "ConfirmedObservation",
                    "evidence": confirmed.geographic_evidence
                }),
            }),
        );
    }
}

#[cfg(test)]
fn find_product(items: &[Value], id: u64) -> Result<&Value, String> {
    if id == 0 {
        return Err("product id must be positive".to_owned());
    }
    items
        .iter()
        .find(|item| item.get("id").and_then(Value::as_u64) == Some(id))
        .ok_or_else(|| format!("product id {id} was not found"))
}

pub fn search_arguments(query: &str, limit: u8) -> Result<Value, String> {
    if query.trim().is_empty() {
        return Err("query must not be empty".to_owned());
    }
    if !(1..=10).contains(&limit) {
        return Err("limit must be in 1..=10".to_owned());
    }
    Ok(json!({
        "q": query,
        "page": 1,
        "limit": limit,
        "sort": "popularity",
        "vvonly": 0,
        "mode": "custom",
        "fields": ["id", "xml_id", "name", "price", "unit", "weight", "rating", "url", "category"]
    }))
}

pub fn unwrap_tool_data(result: &Value) -> Result<Value, String> {
    let parsed: ToolResult = serde_json::from_value(result.clone())
        .map_err(|error| format!("invalid tools/call result: {error}"))?;
    if parsed.is_error {
        return Err("MCP tool reported isError=true".to_owned());
    }
    if let Some(structured) = parsed.structured_content {
        if structured.get("ok").and_then(Value::as_bool) == Some(true) {
            return structured
                .get("data")
                .cloned()
                .ok_or_else(|| "structured tool envelope has no data".to_owned());
        }
        return Ok(structured);
    }
    let text = parsed
        .content
        .iter()
        .find(|part| part.kind == "text")
        .ok_or("MCP tool returned no text content")?;
    let envelope: Value =
        serde_json::from_str(&text.text).map_err(|error| format!("invalid tool JSON: {error}"))?;
    if envelope.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(format!("tool failure: {envelope}"));
    }
    envelope
        .get("data")
        .cloned()
        .ok_or_else(|| "tool envelope has no data".to_owned())
}

pub fn compare_search_results(query: &str, first: &Value, second: &Value) -> Value {
    let compact = |value: &Value| {
        value["items"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|item| {
                json!({
                    "id": item.get("id"),
                    "price": item.get("price").and_then(|price| price.get("current"))
                })
            })
            .collect::<Vec<_>>()
    };
    let first_compact = compact(first);
    let second_compact = compact(second);
    json!({
        "query": query,
        "same_ids_and_prices": first_compact == second_compact,
        "first": first_compact,
        "second": second_compact
    })
}

pub fn collect_sample(client: &mut McpClient) -> Result<SampleFile, String> {
    let queries = [
        "молоко",
        "сыр",
        "гречка",
        "макароны",
        "хлеб",
        "помидоры",
        "яблоки",
        "курица",
        "лосось",
        "кофе",
    ];
    let collected_at = Utc::now();
    let mut observations = Vec::with_capacity(30);
    for query in queries {
        let data = unwrap_tool_data(
            &client.call_read_only_tool("vkusvill_products_search", search_arguments(query, 3)?)?,
        )?;
        for mut item in data["items"].as_array().cloned().unwrap_or_default() {
            normalize_public_item(&mut item, query, collected_at);
            observations.push(item);
        }
    }
    let mut empty_category_checks = Vec::new();
    for query in [
        "презервативы",
        "Contex",
        "Durex",
        "алкогольное вино",
        "водка коньяк виски",
    ] {
        let data = unwrap_tool_data(
            &client.call_read_only_tool("vkusvill_products_search", search_arguments(query, 5)?)?,
        )?;
        empty_category_checks.push(json!({
            "query": query,
            "total": data["meta"]["total"],
            "returned_names": data["items"].as_array().into_iter().flatten()
                .filter_map(|item| item.get("name").cloned()).collect::<Vec<_>>(),
            "interpretation": if matches!(query, "алкогольное вино" | "водка коньяк виски") {
                "Results were reviewed for real alcoholic drinks; unrelated and non-alcoholic matches do not prove alcoholic catalog coverage."
            } else {
                "No public search results."
            }
        }));
    }
    Ok(SampleFile {
        generated_at: Utc::now(),
        source: DEFAULT_ENDPOINT,
        protocol: "MCP Streamable HTTP (negotiated 2025-03-26)",
        binding: "Generic",
        binding_reason: "Anonymous response says personalized=false and does not include availability or a selected store/address.",
        observations,
        empty_category_checks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_result() -> Value {
        serde_json::from_str(include_str!("fixtures/tool_result.json")).unwrap()
    }

    #[test]
    fn validates_search_limit() {
        assert!(search_arguments("молоко", 1).is_ok());
        assert!(search_arguments("молоко", 10).is_ok());
        assert!(search_arguments("молоко", 0).is_err());
        assert!(search_arguments("молоко", 11).is_err());
    }

    #[test]
    fn rejects_empty_query() {
        assert!(search_arguments("  ", 5).is_err());
    }

    #[test]
    fn unwraps_nested_tool_envelope() {
        let data = unwrap_tool_data(&fixture_result()).unwrap();
        assert_eq!(data["items"][0]["id"], 36296);
    }

    #[test]
    fn preserves_current_and_old_price() {
        let data = unwrap_tool_data(&fixture_result()).unwrap();
        assert_eq!(data["items"][0]["price"]["current"], 100);
        assert_eq!(data["items"][0]["price"]["old"], 120);
    }

    #[test]
    fn parses_mass_volume_and_pieces() {
        assert_eq!(
            parse_package("Рис, 450 г").unwrap().normalized_quantity,
            "0.45"
        );
        assert_eq!(
            parse_package("Молоко, 900&nbsp;мл")
                .unwrap()
                .normalized_unit,
            "l"
        );
        assert_eq!(
            parse_package("Яйца 10 шт").unwrap().normalized_unit,
            "piece"
        );
    }

    #[test]
    fn parses_multipacks() {
        let package = parse_package("Рис в пакетиках 5х80 г").unwrap();
        assert_eq!(package.amount, "400");
        assert_eq!(package.normalized_quantity, "0.4");
        assert_eq!(package.basis, "product_name_multipack");
        let pieces = parse_package("Батончики 6×2 шт").unwrap();
        assert_eq!(pieces.normalized_quantity, "12");
    }

    #[test]
    fn rejects_malformed_package() {
        assert!(parse_package("Товар без размера").is_none());
        assert!(parse_package("Товар 0 г").is_none());
    }

    #[test]
    fn distinguishes_binding_states() {
        assert_ne!(
            GeographicBinding::StoreBound,
            GeographicBinding::AddressBound
        );
        assert_ne!(GeographicBinding::AddressBound, GeographicBinding::Generic);
    }

    #[test]
    fn anonymous_item_is_generic_and_has_no_store() {
        let mut item = unwrap_tool_data(&fixture_result()).unwrap()["items"][0].clone();
        normalize_public_item(&mut item, "молоко", Utc::now());
        assert_eq!(item["authorization_mode"], "Anonymous");
        assert_eq!(item["store_binding"], "Generic");
        assert!(item["store_id"].is_null());
    }

    #[test]
    fn reports_missing_and_invalid_product_ids() {
        let items = unwrap_tool_data(&fixture_result()).unwrap()["items"]
            .as_array()
            .unwrap()
            .clone();
        assert!(find_product(&items, 0).is_err());
        assert!(find_product(&items, 999).is_err());
        assert_eq!(find_product(&items, 36296).unwrap()["id"], 36296);
    }

    #[test]
    fn accepts_structured_tool_content() {
        let result = json!({
            "content": [],
            "structuredContent": {"ok": true, "data": {"items": [{"id": 7}]}},
            "isError": false
        });
        assert_eq!(unwrap_tool_data(&result).unwrap()["items"][0]["id"], 7);
    }

    #[test]
    fn rejects_incompatible_tool_schema() {
        let result = json!({"content": [{"type": "image"}], "isError": false});
        assert!(unwrap_tool_data(&result).is_err());
    }

    #[test]
    fn normalizes_rubles_per_kg_liter_and_piece() {
        for (name, expected_value, expected_unit) in [
            ("Крупа, 450 г", "222.22", "RUB/kg"),
            ("Молоко, 900 мл", "111.11", "RUB/l"),
            ("Яйца, 10 шт", "10", "RUB/piece"),
        ] {
            let mut item = json!({"name": name, "price": {"current": 100}, "unit": "шт"});
            normalize_public_item(&mut item, "test", Utc::now());
            assert_eq!(item["normalized_price"]["value"], expected_value);
            assert_eq!(item["normalized_price"]["unit"], expected_unit);
        }
    }

    #[test]
    fn phase06_artifacts_contain_no_secret_assignments() {
        let artifacts = [
            include_str!("../../research/phase06_report.md"),
            include_str!("../../research/phase06_capabilities.json"),
            include_str!("../../research/phase06_sample_products.json"),
        ];
        for artifact in artifacts {
            for forbidden in [
                "access_token=",
                "refresh_token=",
                "client_secret=",
                "authorization_code=",
                "Bearer eyJ",
                "\"token\":",
            ] {
                assert!(!artifact.contains(forbidden), "found {forbidden}");
            }
        }
    }

    #[test]
    fn compares_repeatable_searches() {
        let data = unwrap_tool_data(&fixture_result()).unwrap();
        assert_eq!(
            compare_search_results("молоко", &data, &data)["same_ids_and_prices"],
            true
        );
    }

    #[test]
    fn detects_changed_search_price() {
        let first = unwrap_tool_data(&fixture_result()).unwrap();
        let mut second = first.clone();
        second["items"][0]["price"]["current"] = json!(101);
        assert_eq!(
            compare_search_results("молоко", &first, &second)["same_ids_and_prices"],
            false
        );
    }
}
