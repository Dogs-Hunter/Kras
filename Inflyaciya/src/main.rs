use chrono::{DateTime, NaiveDate, Utc};
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, CONTENT_TYPE, USER_AGENT};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::{env, fs, process, str::FromStr, time::Duration};

mod audit;
mod basket;
mod catalog;
mod domain;
mod export;
mod index;
mod mcp;
mod phase1;
mod phase2;
mod phase21;
mod scheduler;
mod source;
mod storage;
mod weekly;

const DEFAULT_SAMPLE_PATH: &str = "research/sample_products.json";
const DEFAULT_PHASE05_PATH: &str = "research/phase05_sample_products.json";
const USER_AGENT_VALUE: &str = "inflyaciya-phase0/0.1 (educational low-volume availability probe)";

#[derive(Debug, Deserialize)]
struct SampleFile {
    observations: Vec<RawObservation>,
}

#[derive(Debug, Deserialize)]
struct RawObservation {
    product_id: Option<String>,
    name: String,
    brand: Option<String>,
    category: String,
    group: String,
    basket_scope: String,
    url: String,
    package_price: String,
    regular_price: Option<String>,
    discount_price: Option<String>,
    package: Package,
    availability: Option<String>,
    store_id: Option<String>,
    store_binding_confirmed: bool,
    collected_at: DateTime<Utc>,
    source_kind: String,
    collection_note: String,
}

#[derive(Debug, Deserialize)]
struct Package {
    kind: UnitKind,
    value: String,
    display: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum UnitKind {
    MassG,
    VolumeMl,
    Pieces,
}

#[derive(Debug, Serialize)]
struct NormalizedObservation {
    product_id: Option<String>,
    name: String,
    brand: Option<String>,
    category: String,
    group: String,
    basket_scope: String,
    url: String,
    package_price: String,
    regular_price: Option<String>,
    discount_price: Option<String>,
    package_display: String,
    normalized_price: String,
    normalized_unit: &'static str,
    availability: Option<String>,
    store_id: Option<String>,
    store_binding_confirmed: bool,
    collected_at: DateTime<Utc>,
    source_kind: String,
    collection_note: String,
}

#[derive(Debug, Serialize)]
struct ProbeResult {
    url: String,
    checked_at: DateTime<Utc>,
    outcome: &'static str,
    http_status: Option<u16>,
    content_type: Option<String>,
    response_bytes: Option<usize>,
    detail: String,
}

#[derive(Debug, Serialize)]
struct RunOutput {
    generated_at: DateTime<Utc>,
    probe_results: Vec<ProbeResult>,
    observations: Vec<NormalizedObservation>,
}

#[derive(Debug, Deserialize)]
struct Phase05File {
    decision: String,
    confirmed_observations: Vec<Phase05Observation>,
    unconfirmed_observations: Vec<Phase05Observation>,
}

#[derive(Debug, Deserialize)]
struct Phase05Observation {
    source: String,
    store_id: Option<String>,
    store_address: Option<String>,
    store_binding_confirmed: bool,
    product_id: Option<String>,
    category: String,
    group: String,
    name: String,
    url: String,
    package_price: String,
    package_amount: String,
    unit: String,
    normalized_price: String,
    price_type: String,
    offer_valid_from: Option<NaiveDate>,
    offer_valid_to: Option<NaiveDate>,
    collected_at: DateTime<Utc>,
    evidence: String,
}

#[derive(Debug, Serialize)]
struct Phase05ValidationSummary {
    validated_at: DateTime<Utc>,
    decision: String,
    confirmed_count: usize,
    unconfirmed_count: usize,
    input_path: String,
    result: &'static str,
}

fn parse_decimal(field: &str, value: &str) -> Result<Decimal, String> {
    Decimal::from_str(value).map_err(|error| format!("invalid {field}={value:?}: {error}"))
}

fn normalize(price: Decimal, package: &Package) -> Result<(Decimal, &'static str), String> {
    let value = parse_decimal("package.value", &package.value)?;
    if value <= Decimal::ZERO {
        return Err("package.value must be positive".to_owned());
    }
    match package.kind {
        UnitKind::MassG => Ok((price * Decimal::from(1000u16) / value, "RUB/kg")),
        UnitKind::VolumeMl => Ok((price * Decimal::from(1000u16) / value, "RUB/l")),
        UnitKind::Pieces => Ok((price / value, "RUB/piece")),
    }
}

fn load_and_normalize(path: &str) -> Result<Vec<NormalizedObservation>, String> {
    let text = fs::read_to_string(path).map_err(|error| format!("cannot read {path}: {error}"))?;
    let input: SampleFile =
        serde_json::from_str(&text).map_err(|error| format!("invalid JSON in {path}: {error}"))?;
    input
        .observations
        .into_iter()
        .map(|raw| {
            let price = parse_decimal("package_price", &raw.package_price)?;
            let (normalized, unit) = normalize(price, &raw.package)?;
            Ok(NormalizedObservation {
                product_id: raw.product_id,
                name: raw.name,
                brand: raw.brand,
                category: raw.category,
                group: raw.group,
                basket_scope: raw.basket_scope,
                url: raw.url,
                package_price: price.round_dp(2).to_string(),
                regular_price: raw.regular_price,
                discount_price: raw.discount_price,
                package_display: raw.package.display,
                normalized_price: normalized.round_dp(2).to_string(),
                normalized_unit: unit,
                availability: raw.availability,
                store_id: raw.store_id,
                store_binding_confirmed: raw.store_binding_confirmed,
                collected_at: raw.collected_at,
                source_kind: raw.source_kind,
                collection_note: raw.collection_note,
            })
        })
        .collect()
}

fn validate_phase05_observation(
    observation: &Phase05Observation,
    expected_confirmed: bool,
) -> Result<(), String> {
    let required_text = [
        ("source", observation.source.as_str()),
        ("category", observation.category.as_str()),
        ("group", observation.group.as_str()),
        ("name", observation.name.as_str()),
        ("url", observation.url.as_str()),
        ("price_type", observation.price_type.as_str()),
        ("evidence", observation.evidence.as_str()),
    ];
    for (field, value) in required_text {
        if value.trim().is_empty() {
            return Err(format!("{}: {field} must not be empty", observation.name));
        }
    }
    if observation.store_binding_confirmed != expected_confirmed {
        return Err(format!(
            "{}: store_binding_confirmed={} conflicts with its dataset section",
            observation.name, observation.store_binding_confirmed
        ));
    }
    if expected_confirmed {
        if observation.store_id.as_deref() != Some("B153") {
            return Err(format!(
                "{}: confirmed record must use store_id B153",
                observation.name
            ));
        }
        let address = observation.store_address.as_deref().unwrap_or_default();
        if !address.contains("Щукин") || !address.contains("42") {
            return Err(format!(
                "{}: confirmed record must name Щукинская, 42",
                observation.name
            ));
        }
    }
    if observation.product_id.as_deref().is_some_and(str::is_empty) {
        return Err(format!(
            "{}: empty product_id must be null",
            observation.name
        ));
    }
    let package_price = parse_decimal("package_price", &observation.package_price)?;
    let package_amount = parse_decimal("package_amount", &observation.package_amount)?;
    let normalized_price = parse_decimal("normalized_price", &observation.normalized_price)?;
    if package_price <= Decimal::ZERO
        || package_amount <= Decimal::ZERO
        || normalized_price <= Decimal::ZERO
    {
        return Err(format!(
            "{}: prices and amount must be positive",
            observation.name
        ));
    }
    let expected_normalized = match observation.unit.as_str() {
        "g" => package_price * Decimal::from(1000u16) / package_amount,
        "ml" => package_price * Decimal::from(1000u16) / package_amount,
        "piece" => package_price / package_amount,
        "kg" | "l" => package_price / package_amount,
        other => return Err(format!("{}: unsupported unit {other:?}", observation.name)),
    };
    if expected_normalized.round_dp(2) != normalized_price.round_dp(2) {
        return Err(format!(
            "{}: normalized_price does not match package price and amount",
            observation.name
        ));
    }
    if let (Some(from), Some(to)) = (observation.offer_valid_from, observation.offer_valid_to)
        && from > to
    {
        return Err(format!(
            "{}: offer date range is reversed",
            observation.name
        ));
    }
    if observation.collected_at > Utc::now() + chrono::Duration::minutes(5) {
        return Err(format!(
            "{}: collected_at is in the future",
            observation.name
        ));
    }
    Ok(())
}

fn run_phase05(path: &str) -> Result<(), String> {
    let text = fs::read_to_string(path).map_err(|error| format!("cannot read {path}: {error}"))?;
    let input: Phase05File =
        serde_json::from_str(&text).map_err(|error| format!("invalid JSON in {path}: {error}"))?;
    for observation in &input.confirmed_observations {
        validate_phase05_observation(observation, true)?;
    }
    for observation in &input.unconfirmed_observations {
        validate_phase05_observation(observation, false)?;
    }
    let summary = Phase05ValidationSummary {
        validated_at: Utc::now(),
        decision: input.decision,
        confirmed_count: input.confirmed_observations.len(),
        unconfirmed_count: input.unconfirmed_observations.len(),
        input_path: path.to_owned(),
        result: "valid",
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn probe(client: &Client, url: &str) -> ProbeResult {
    let checked_at = Utc::now();
    match client
        .get(url)
        .header(USER_AGENT, USER_AGENT_VALUE)
        .header(ACCEPT, "text/html,application/json;q=0.9,*/*;q=0.1")
        .send()
    {
        Ok(response) => {
            let status = response.status();
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(ToOwned::to_owned);
            match response.bytes() {
                Ok(body) => {
                    let body_text = String::from_utf8_lossy(&body).to_lowercase();
                    let challenge_shell = body_text.contains("servicepipe.tech")
                        || body_text.contains("имеем дело именно с вами")
                        || body_text.contains("captcha");
                    let (outcome, detail) = if status.as_u16() == 403 {
                        (
                            "blocked_or_error",
                            "HTTP 403; the prototype does not bypass anti-bot protection"
                                .to_owned(),
                        )
                    } else if challenge_shell {
                        (
                            "challenge_shell",
                            format!(
                                "HTTP {status}, but the body is an anti-bot JavaScript/challenge shell, not catalog data"
                            ),
                        )
                    } else if status.is_success() {
                        ("received", format!("HTTP {status}"))
                    } else {
                        ("blocked_or_error", format!("HTTP {status}"))
                    };
                    ProbeResult {
                        url: url.to_owned(),
                        checked_at,
                        outcome,
                        http_status: Some(status.as_u16()),
                        content_type,
                        response_bytes: Some(body.len()),
                        detail,
                    }
                }
                Err(error) => ProbeResult {
                    url: url.to_owned(),
                    checked_at,
                    outcome: "read_error",
                    http_status: Some(status.as_u16()),
                    content_type,
                    response_bytes: None,
                    detail: error.to_string(),
                },
            }
        }
        Err(error) => ProbeResult {
            url: url.to_owned(),
            checked_at,
            outcome: "request_error",
            http_status: None,
            content_type: None,
            response_bytes: None,
            detail: error.to_string(),
        },
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().collect();
    if args.get(1).is_some_and(|value| value == "collect")
        && args.get(2).is_some_and(|value| value == "weekly")
    {
        return phase2::run(&args[1..]);
    }
    if args.get(1).is_some_and(|value| value == "basket") {
        return phase2::run(&args[1..]);
    }
    if args
        .get(1)
        .is_some_and(|value| matches!(value.as_str(), "--help" | "-h"))
    {
        phase2::print_help();
        return Ok(());
    }
    if args
        .get(1)
        .is_some_and(|value| phase2::is_phase2_command(value))
    {
        return phase2::run(&args[1..]);
    }
    if args
        .get(1)
        .is_some_and(|value| phase1::is_phase1_command(value))
    {
        return phase1::run(&args[1..]);
    }
    if args.get(1).is_some_and(|value| value == "mcp") {
        return mcp::run_cli(&args[2..]);
    }
    if args.get(1).is_some_and(|value| value == "--phase05") {
        let path = args
            .get(2)
            .map(String::as_str)
            .unwrap_or(DEFAULT_PHASE05_PATH);
        return run_phase05(path);
    }
    let sample_path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| DEFAULT_SAMPLE_PATH.to_owned());
    let client = Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|error| error.to_string())?;
    let urls = [
        "https://kuper.ru/perekrestok",
        "https://kuper.ru/pickup",
        "https://kuper.ru/perekrestok/c/skidki-i-aktsii/skidki/alkogol-cc137d2",
        "https://www.perekrestok.ru/shops",
        "https://www.perekrestok.ru/booklets",
        "https://www.perekrestok.ru/cat/c/99/prezervativy-smazki",
        "https://www.perekrestok.ru/cat/c/9/pivo",
    ];
    let output = RunOutput {
        generated_at: Utc::now(),
        probe_results: urls.iter().map(|url| probe(&client, url)).collect(),
        observations: load_and_normalize(&sample_path)?,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&output).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn package(kind: UnitKind, value: &str) -> Package {
        Package {
            kind,
            value: value.to_owned(),
            display: String::new(),
        }
    }

    #[test]
    fn normalizes_900_grams() {
        assert_eq!(
            normalize(dec!(90), &package(UnitKind::MassG, "900")).unwrap(),
            (dec!(100), "RUB/kg")
        );
    }

    #[test]
    fn normalizes_750_milliliters() {
        assert_eq!(
            normalize(dec!(600), &package(UnitKind::VolumeMl, "750")).unwrap(),
            (dec!(800), "RUB/l")
        );
    }

    #[test]
    fn normalizes_piece_count() {
        assert_eq!(
            normalize(dec!(360), &package(UnitKind::Pieces, "12")).unwrap(),
            (dec!(30), "RUB/piece")
        );
    }

    #[test]
    fn rejects_zero_quantity() {
        assert!(normalize(dec!(10), &package(UnitKind::MassG, "0")).is_err());
    }

    fn phase05_observation(confirmed: bool) -> Phase05Observation {
        Phase05Observation {
            source: "manual_receipt".to_owned(),
            store_id: confirmed.then(|| "B153".to_owned()),
            store_address: confirmed.then(|| "Москва, Щукинская улица, 42".to_owned()),
            store_binding_confirmed: confirmed,
            product_id: Some("test-id".to_owned()),
            category: "Молочные продукты".to_owned(),
            group: "Молоко".to_owned(),
            name: "Тестовая запись схемы".to_owned(),
            url: "https://example.invalid/evidence".to_owned(),
            package_price: "90".to_owned(),
            package_amount: "900".to_owned(),
            unit: "ml".to_owned(),
            normalized_price: "100".to_owned(),
            price_type: "receipt_purchase".to_owned(),
            offer_valid_from: None,
            offer_valid_to: None,
            collected_at: Utc::now(),
            evidence: "Синтетическая запись только для unit-теста схемы".to_owned(),
        }
    }

    #[test]
    fn validates_confirmed_b153_manual_record() {
        validate_phase05_observation(&phase05_observation(true), true).unwrap();
    }

    #[test]
    fn rejects_unbound_record_in_confirmed_section() {
        let observation = phase05_observation(false);
        assert!(validate_phase05_observation(&observation, true).is_err());
    }
}
