use chrono::{DateTime, Utc};
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::str::FromStr;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Product {
    pub source_product_id: String,
    pub mcp_product_id: u64,
    pub xml_id: Option<u64>,
    pub name: String,
    pub brand: Option<String>,
    pub category: String,
    pub group: String,
    pub query: String,
    pub url: String,
    pub package_price: String,
    pub old_price: Option<String>,
    pub discount_percent: Option<String>,
    pub discount_info: Option<String>,
    pub package_amount: String,
    pub package_unit: String,
    pub package_basis: String,
    pub normalized_quantity: String,
    pub normalized_unit: String,
    pub normalized_price: String,
    pub normalized_price_unit: String,
    pub collected_at: DateTime<Utc>,
    pub source: String,
    pub price_context: String,
    pub availability_status: String,
    pub validation_status: String,
}

impl Product {
    pub fn package_price_decimal(&self) -> Result<Decimal, String> {
        Decimal::from_str(&self.package_price)
            .map_err(|error| format!("invalid package price: {error}"))
    }

    pub fn package_fingerprint(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.package_amount, self.package_unit, self.normalized_quantity, self.normalized_unit
        )
    }

    pub fn from_normalized_value(value: &Value, group: &GroupSpec) -> Result<Self, String> {
        let id = value
            .get("id")
            .and_then(Value::as_u64)
            .ok_or("product has no numeric id")?;
        let name = value
            .get("name")
            .and_then(Value::as_str)
            .ok_or("product has no name")?;
        let price = decimal_value(&value["price"]["current"])?;
        let package = value.get("package").ok_or("product has no package")?;
        let normalized_price = value
            .get("normalized_price")
            .ok_or("product has no normalized price")?;
        let normalized_unit = package["normalized_unit"]
            .as_str()
            .ok_or("package has no normalized unit")?;
        if !matches!(normalized_unit, "kg" | "l" | "piece") {
            return Err(format!("unsupported normalized unit {normalized_unit:?}"));
        }
        let category = value
            .get("category")
            .and_then(Value::as_array)
            .and_then(|path| path.first())
            .and_then(|entry| entry.get("name"))
            .and_then(Value::as_str)
            .unwrap_or(&group.category)
            .to_owned();
        Ok(Self {
            source_product_id: format!("vkusvill:{id}"),
            mcp_product_id: id,
            xml_id: value.get("xml_id").and_then(Value::as_u64),
            name: decode_html(name),
            brand: value.get("brand").and_then(Value::as_str).map(decode_html),
            category,
            group: group.group.clone(),
            query: group.query.clone(),
            url: value
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            package_price: price.normalize().to_string(),
            old_price: decimal_optional_value(&value["price"]["old"]),
            discount_percent: decimal_optional_value(&value["price"]["discount_percent"]),
            discount_info: value["price"]["discount_info"]
                .as_str()
                .map(ToOwned::to_owned),
            package_amount: package["amount"]
                .as_str()
                .ok_or("package has no amount")?
                .to_owned(),
            package_unit: package["unit"]
                .as_str()
                .ok_or("package has no unit")?
                .to_owned(),
            package_basis: package["basis"]
                .as_str()
                .ok_or("package has no basis")?
                .to_owned(),
            normalized_quantity: package["normalized_quantity"]
                .as_str()
                .ok_or("package has no normalized quantity")?
                .to_owned(),
            normalized_unit: normalized_unit.to_owned(),
            normalized_price: normalized_price["value"]
                .as_str()
                .ok_or("normalized price has no value")?
                .to_owned(),
            normalized_price_unit: normalized_price["unit"]
                .as_str()
                .ok_or("normalized price has no unit")?
                .to_owned(),
            collected_at: value
                .get("collected_at")
                .and_then(Value::as_str)
                .and_then(|raw| raw.parse().ok())
                .unwrap_or_else(Utc::now),
            source: value
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("https://mcp.vkusvill.ru/mcp")
                .to_owned(),
            price_context: "Generic".to_owned(),
            availability_status: "observed_in_anonymous_search".to_owned(),
            validation_status: "CandidateObservation".to_owned(),
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GroupSpec {
    pub category: String,
    pub group: String,
    pub query: String,
    pub food: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProductCatalog {
    pub schema_version: u32,
    pub generated_at: String,
    pub source: String,
    pub price_context: String,
    pub products: Vec<Product>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BasketItem {
    pub group: String,
    pub source_product_id: String,
    pub mcp_product_id: u64,
    pub name: String,
    pub url: String,
    pub quantity_packages: u32,
    pub package_price: String,
    pub package_amount: String,
    pub package_unit: String,
    pub package_fingerprint: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BasketConfig {
    pub schema_version: u32,
    pub basket_id: String,
    pub status: String,
    pub created_at: String,
    pub approved_at: String,
    pub minimum_wage_rub: String,
    pub target_rub: String,
    pub minimum_rub: String,
    pub maximum_rub: String,
    pub total_rub: String,
    pub items: Vec<BasketItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    pub source: SourceSettings,
    pub collection: CollectionSettings,
    pub budget: BudgetSettings,
    pub policy: PolicySettings,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceSettings {
    pub id: String,
    pub endpoint: String,
    pub access_mode: String,
    pub price_context: String,
    pub request_timeout_seconds: u64,
    pub max_retries: u32,
    pub backoff_milliseconds: u64,
    pub page_limit: u8,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CollectionSettings {
    pub frequency_days: u32,
    pub database_path: String,
    pub products_path: String,
    pub basket_path: String,
    pub output_directory: String,
    pub logs_directory: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BudgetSettings {
    pub region: String,
    pub year: u32,
    pub minimum_wage_rub: String,
    pub share: String,
    pub tolerance: String,
    pub normative_source: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PolicySettings {
    pub food_only_main_basket: bool,
    pub require_normalized_kg_or_l: bool,
    pub one_product_per_group: bool,
    pub allow_automatic_replacement_after_approval: bool,
    pub allow_ordering_tools: bool,
}

#[derive(Debug, Clone)]
pub struct Budget {
    pub minimum_wage: Decimal,
    pub target: Decimal,
    pub minimum: Decimal,
    pub maximum: Decimal,
}

impl BudgetSettings {
    pub fn calculate(&self) -> Result<Budget, String> {
        let minimum_wage = Decimal::from_str(&self.minimum_wage_rub)
            .map_err(|error| format!("invalid minimum wage: {error}"))?;
        let share = Decimal::from_str(&self.share)
            .map_err(|error| format!("invalid budget share: {error}"))?;
        let tolerance = Decimal::from_str(&self.tolerance)
            .map_err(|error| format!("invalid tolerance: {error}"))?;
        let target = (minimum_wage * share).round_dp(2);
        let delta = target * tolerance;
        Ok(Budget {
            minimum_wage,
            target,
            minimum: (target - delta)
                .round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero),
            maximum: (target + delta)
                .round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero),
        })
    }
}

fn decimal_value(value: &Value) -> Result<Decimal, String> {
    Decimal::from_str(&value.to_string()).map_err(|error| format!("invalid decimal: {error}"))
}

fn decimal_optional_value(value: &Value) -> Option<String> {
    (!value.is_null())
        .then(|| decimal_value(value).ok())
        .flatten()
        .map(|value| value.normalize().to_string())
}

fn decode_html(value: &str) -> String {
    value
        .replace("&nbsp;", " ")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculates_budget_from_configuration() {
        let settings = BudgetSettings {
            region: "Москва".to_owned(),
            year: 2026,
            minimum_wage_rub: "39730.00".to_owned(),
            share: "0.25".to_owned(),
            tolerance: "0.05".to_owned(),
            normative_source: "official".to_owned(),
        };
        let budget = settings.calculate().unwrap();
        assert_eq!(budget.target.to_string(), "9932.50");
        assert_eq!(budget.minimum.to_string(), "9435.88");
        assert_eq!(budget.maximum.to_string(), "10429.13");
    }

    #[test]
    fn package_fingerprint_changes_with_size() {
        let mut product: Product = toml::from_str(
            r#"source_product_id="vkusvill:1"
mcp_product_id=1
name="Товар"
category="Категория"
group="Группа"
query="товар"
url="https://example.invalid"
package_price="100"
package_amount="500"
package_unit="г"
package_basis="product_name"
normalized_quantity="0.5"
normalized_unit="kg"
normalized_price="200"
normalized_price_unit="RUB/kg"
collected_at="2026-10-10T00:00:00Z"
source="fixture"
price_context="Generic"
availability_status="observed"
validation_status="CandidateObservation""#,
        )
        .unwrap();
        let before = product.package_fingerprint();
        product.package_amount = "450".to_owned();
        assert_ne!(before, product.package_fingerprint());
    }
}
