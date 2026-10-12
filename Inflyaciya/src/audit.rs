use chrono::Utc;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs, str::FromStr};

use crate::{
    basket,
    catalog::load_catalog,
    domain::{BasketConfig, Product, Settings},
    mcp::{
        client::McpClient,
        products::{normalize_public_item, unwrap_tool_data},
    },
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EvidenceLevel {
    SourceExplicit,
    SourceText,
    Calculated,
    Estimated,
    Unverified,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PriceBasis {
    FixedPackagePrice,
    UnitWeightPrice,
    UnitVolumePrice,
    PiecePrice,
    VariableWeightPrice,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AuditStatus {
    Verified,
    VerifiedWithConditions,
    RequiresReview,
    Invalid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditItem {
    pub group: String,
    pub source_product_id: String,
    pub mcp_product_id: u64,
    pub name: String,
    pub category: String,
    pub url: String,
    pub original_price: String,
    pub current_price: Option<String>,
    pub sale_unit: Option<String>,
    pub source_weight: Option<String>,
    pub source_weight_unit: Option<String>,
    pub price_basis: PriceBasis,
    pub price_basis_evidence: EvidenceLevel,
    pub quantity_evidence: EvidenceLevel,
    pub normalized_price: String,
    pub normalized_price_recalculated: Option<String>,
    pub normalized_price_matches: bool,
    pub promotion_active: bool,
    pub old_price: Option<String>,
    pub discount_percent: Option<String>,
    pub discount_info: Option<String>,
    pub identity_matches: bool,
    pub package_matches: bool,
    pub suitable_for_fixed_index: bool,
    pub status: AuditStatus,
    pub issues: Vec<String>,
    pub observed_at: String,
    pub raw_details: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Concentration {
    pub top_four_rub: String,
    pub top_four_share: String,
    pub top_ten_share: String,
    pub hhi: String,
    pub largest_item_sensitivity_10_percent_rub: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasketAudit {
    pub schema_version: u32,
    pub generated_at: String,
    pub basket_id: String,
    pub basket_sha256: String,
    pub status: String,
    pub arithmetic_total_rub: String,
    pub declared_total_rub: String,
    pub target_rub: String,
    pub deviation_rub: String,
    pub within_tolerance: bool,
    pub item_count: usize,
    pub unique_groups: usize,
    pub duplicate_product_ids: Vec<String>,
    pub verified: usize,
    pub verified_with_conditions: usize,
    pub requires_review: usize,
    pub invalid: usize,
    pub concentration: Concentration,
    #[serde(default)]
    pub item_cost_shares: Vec<CostShare>,
    #[serde(default)]
    pub category_cost_shares: Vec<CostShare>,
    pub items: Vec<AuditItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostShare {
    pub key: String,
    pub cost_rub: String,
    pub share_percent: String,
}

pub fn sha256_file(path: &str) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|error| format!("не удалось прочитать {path}: {error}"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)).to_uppercase())
}

pub fn recalculate_total(basket: &BasketConfig) -> Result<Decimal, String> {
    basket.items.iter().try_fold(Decimal::ZERO, |sum, item| {
        let price = Decimal::from_str(&item.package_price).map_err(|error| error.to_string())?;
        Ok(sum + price * Decimal::from(item.quantity_packages))
    })
}

#[allow(dead_code)]
pub fn run_live(settings: &Settings) -> Result<BasketAudit, String> {
    let audit = run_live_for_path(settings, &settings.collection.basket_path)?;
    save_reports(&audit)?;
    Ok(audit)
}

pub fn run_live_for_path(settings: &Settings, basket_path: &str) -> Result<BasketAudit, String> {
    let basket = basket::load(basket_path)?;
    let catalog = load_catalog(&settings.collection.products_path)?;
    let original_by_id = catalog
        .products
        .iter()
        .map(|product| (product.source_product_id.as_str(), product))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut client = McpClient::with_policy(
        &settings.source.endpoint,
        settings.source.request_timeout_seconds,
        settings.source.max_retries,
        settings.source.backoff_milliseconds,
    )?;
    client.initialize()?;
    client.initialized()?;
    let mut items = Vec::new();
    for basket_item in &basket.items {
        let observed_at = Utc::now();
        let response = client.call_read_only_tool(
            "vkusvill_product_details",
            json!({"id": basket_item.mcp_product_id}),
        );
        let audit = match response.and_then(|value| unwrap_tool_data(&value)) {
            Ok(details) => audit_details(
                basket_item,
                original_by_id
                    .get(basket_item.source_product_id.as_str())
                    .copied(),
                details,
                observed_at.to_rfc3339(),
            ),
            Err(error) => failed_item(basket_item, error, observed_at.to_rfc3339()),
        };
        items.push(audit);
    }
    client.close()?;
    assemble_audit(basket_path, &basket, items)
}

fn failed_item(item: &crate::domain::BasketItem, error: String, observed_at: String) -> AuditItem {
    AuditItem {
        group: item.group.clone(),
        source_product_id: item.source_product_id.clone(),
        mcp_product_id: item.mcp_product_id,
        name: item.name.clone(),
        category: String::new(),
        url: item.url.clone(),
        original_price: item.package_price.clone(),
        current_price: None,
        sale_unit: None,
        source_weight: None,
        source_weight_unit: None,
        price_basis: PriceBasis::Unknown,
        price_basis_evidence: EvidenceLevel::Unverified,
        quantity_evidence: EvidenceLevel::Unverified,
        normalized_price: String::new(),
        normalized_price_recalculated: None,
        normalized_price_matches: false,
        promotion_active: false,
        old_price: None,
        discount_percent: None,
        discount_info: None,
        identity_matches: false,
        package_matches: false,
        suitable_for_fixed_index: false,
        status: AuditStatus::RequiresReview,
        issues: vec![format!("карточка не получена: {error}")],
        observed_at,
        raw_details: Value::Null,
    }
}

fn audit_details(
    item: &crate::domain::BasketItem,
    original: Option<&Product>,
    mut details: Value,
    observed_at: String,
) -> AuditItem {
    let unit = details
        .get("unit")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let weight = decimal_json(&details["weight"]["value"]);
    let weight_unit = details["weight"]["unit"].as_str().map(str::to_owned);
    let name = details["name"]
        .as_str()
        .unwrap_or_default()
        .replace("&nbsp;", " ");
    let description = details["description"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    let basis = classify_price_basis(unit.as_deref(), weight.as_deref(), &name, &description);
    let quantity_evidence = if item.mcp_product_id == 56310 {
        EvidenceLevel::Estimated
    } else if weight.is_some()
        || matches!(
            basis,
            PriceBasis::UnitWeightPrice
                | PriceBasis::UnitVolumePrice
                | PriceBasis::VariableWeightPrice
        )
    {
        EvidenceLevel::SourceExplicit
    } else {
        EvidenceLevel::Unverified
    };
    let mut normalized = details.clone();
    normalize_public_item(&mut normalized, "audit", Utc::now());
    let recalculated = normalized["normalized_price"]["value"]
        .as_str()
        .map(str::to_owned);
    let original_normalized = original
        .map(|value| value.normalized_price.clone())
        .unwrap_or_default();
    let normalized_matches = recalculated.as_deref() == Some(original_normalized.as_str());
    let current_price = decimal_json(&details["price"]["current"]);
    let old_price = decimal_json(&details["price"]["old"]);
    let discount_percent = decimal_json(&details["price"]["discount_percent"]);
    let mut issues = Vec::new();
    let identity_matches = details["id"].as_u64() == Some(item.mcp_product_id) && name == item.name;
    if !identity_matches {
        issues.push("конфликт идентичности товара".to_owned());
    }
    let package_matches = original.is_some_and(|product| {
        recalculated.as_deref() == Some(product.normalized_price.as_str())
            || current_price.as_deref() == Some(product.package_price.as_str())
    });
    if matches!(basis, PriceBasis::VariableWeightPrice) {
        issues.push(
            "цена задана за килограмм; фиксированная упаковка 1 кг источником не подтверждена"
                .to_owned(),
        );
    }
    if item.mcp_product_id == 56310 {
        issues.push("цена задана за штуку; 0,15 кг — номинальная/ориентировочная масса, а не измерение купленного плода".to_owned());
    }
    if item.mcp_product_id == 37147 && weight_unit.as_deref() == Some("кг") {
        issues.push(
            "масло описано источником в килограммах; перевод в литры без плотности запрещён"
                .to_owned(),
        );
    }
    if !normalized_matches {
        issues
            .push("актуальная нормализованная цена отличается от исходного наблюдения".to_owned());
    }
    let discount_info = details["price"]["discount_info"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| original.and_then(|product| product.discount_info.clone()));
    let promotion_active =
        old_price.is_some() || discount_percent.is_some() || discount_info.is_some();
    if promotion_active {
        issues.push("текущая опубликованная цена имеет акционные признаки".to_owned());
    }
    let mut suitable = identity_matches
        && !matches!(basis, PriceBasis::Unknown | PriceBasis::PiecePrice)
        && item.mcp_product_id != 56310;
    if item.group == "Кофе" && name.to_lowercase().contains("напиток кофейный") {
        issues.push(
            "кофейный напиток не подтверждает соответствие локальной группе чистого кофе"
                .to_owned(),
        );
        suitable = false;
    }
    let status = if !identity_matches || matches!(basis, PriceBasis::Unknown) {
        AuditStatus::Invalid
    } else if !suitable {
        AuditStatus::RequiresReview
    } else if !issues.is_empty() {
        AuditStatus::VerifiedWithConditions
    } else {
        AuditStatus::Verified
    };
    let category = details["category"]
        .as_array()
        .and_then(|values| values.first())
        .and_then(|value| value["name"].as_str())
        .unwrap_or_default()
        .to_owned();
    AuditItem {
        group: item.group.clone(),
        source_product_id: item.source_product_id.clone(),
        mcp_product_id: item.mcp_product_id,
        name,
        category,
        url: details["url"].as_str().unwrap_or(&item.url).to_owned(),
        original_price: item.package_price.clone(),
        current_price,
        sale_unit: unit,
        source_weight: weight,
        source_weight_unit: weight_unit,
        price_basis: basis,
        price_basis_evidence: EvidenceLevel::SourceExplicit,
        quantity_evidence,
        normalized_price: original_normalized,
        normalized_price_recalculated: recalculated,
        normalized_price_matches: normalized_matches,
        promotion_active,
        old_price,
        discount_percent,
        discount_info,
        identity_matches,
        package_matches,
        suitable_for_fixed_index: suitable,
        status,
        issues,
        observed_at,
        raw_details: details.take(),
    }
}

pub fn classify_price_basis(
    unit: Option<&str>,
    weight: Option<&str>,
    name: &str,
    description: &str,
) -> PriceBasis {
    match (unit, weight) {
        (Some("кг"), None) if name.contains("ВЕС") || description.contains("продаётся на вес") => {
            PriceBasis::VariableWeightPrice
        }
        (Some("кг"), _) => PriceBasis::UnitWeightPrice,
        (Some("л"), _) => PriceBasis::UnitVolumePrice,
        (Some("шт"), _) if name.to_lowercase().contains(", шт") => PriceBasis::PiecePrice,
        (Some("шт"), Some(_)) => PriceBasis::FixedPackagePrice,
        _ => PriceBasis::Unknown,
    }
}

fn assemble_audit(
    path: &str,
    basket: &BasketConfig,
    items: Vec<AuditItem>,
) -> Result<BasketAudit, String> {
    let total = recalculate_total(basket)?;
    let target = Decimal::from_str(&basket.target_rub).map_err(|error| error.to_string())?;
    let minimum = Decimal::from_str(&basket.minimum_rub).map_err(|error| error.to_string())?;
    let maximum = Decimal::from_str(&basket.maximum_rub).map_err(|error| error.to_string())?;
    let groups = basket
        .items
        .iter()
        .map(|item| &item.group)
        .collect::<BTreeSet<_>>();
    let mut ids = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    for item in &basket.items {
        if !ids.insert(&item.source_product_id) {
            duplicates.insert(item.source_product_id.clone());
        }
    }
    let concentration = concentration(basket)?;
    let requires_review = items
        .iter()
        .filter(|item| item.status == AuditStatus::RequiresReview)
        .count();
    let invalid = items
        .iter()
        .filter(|item| item.status == AuditStatus::Invalid)
        .count();
    let (item_cost_shares, category_cost_shares) = cost_shares(basket, &items)?;
    Ok(BasketAudit {
        schema_version: 2,
        generated_at: Utc::now().to_rfc3339(),
        basket_id: basket.basket_id.clone(),
        basket_sha256: sha256_file(path)?,
        status: if requires_review + invalid == 0 {
            "BasketVerified"
        } else {
            "BlockedByBasketAudit"
        }
        .to_owned(),
        arithmetic_total_rub: total.to_string(),
        declared_total_rub: basket.total_rub.clone(),
        target_rub: basket.target_rub.clone(),
        deviation_rub: (total - target).to_string(),
        within_tolerance: total >= minimum && total <= maximum,
        item_count: basket.items.len(),
        unique_groups: groups.len(),
        duplicate_product_ids: duplicates.into_iter().collect(),
        verified: items
            .iter()
            .filter(|item| item.status == AuditStatus::Verified)
            .count(),
        verified_with_conditions: items
            .iter()
            .filter(|item| item.status == AuditStatus::VerifiedWithConditions)
            .count(),
        requires_review,
        invalid,
        concentration,
        item_cost_shares,
        category_cost_shares,
        items,
    })
}

fn cost_shares(
    basket: &BasketConfig,
    audit_items: &[AuditItem],
) -> Result<(Vec<CostShare>, Vec<CostShare>), String> {
    let total = recalculate_total(basket)?;
    let categories = audit_items
        .iter()
        .map(|item| (item.source_product_id.as_str(), item.category.as_str()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut item_shares = Vec::new();
    let mut category_costs = std::collections::BTreeMap::<String, Decimal>::new();
    for item in &basket.items {
        let cost = Decimal::from_str(&item.package_price).map_err(|error| error.to_string())?
            * Decimal::from(item.quantity_packages);
        item_shares.push(CostShare {
            key: format!("{} / {}", item.group, item.source_product_id),
            cost_rub: cost.to_string(),
            share_percent: ((cost / total) * Decimal::from(100))
                .round_dp(4)
                .to_string(),
        });
        *category_costs
            .entry(
                categories
                    .get(item.source_product_id.as_str())
                    .copied()
                    .unwrap_or("Не подтверждена")
                    .to_owned(),
            )
            .or_default() += cost;
    }
    item_shares.sort_by(|left, right| {
        Decimal::from_str(&right.cost_rub)
            .unwrap_or_default()
            .cmp(&Decimal::from_str(&left.cost_rub).unwrap_or_default())
    });
    let mut category_shares = category_costs
        .into_iter()
        .map(|(key, cost)| CostShare {
            key,
            cost_rub: cost.to_string(),
            share_percent: ((cost / total) * Decimal::from(100))
                .round_dp(4)
                .to_string(),
        })
        .collect::<Vec<_>>();
    category_shares.sort_by(|left, right| {
        Decimal::from_str(&right.cost_rub)
            .unwrap_or_default()
            .cmp(&Decimal::from_str(&left.cost_rub).unwrap_or_default())
    });
    Ok((item_shares, category_shares))
}

pub fn concentration(basket: &BasketConfig) -> Result<Concentration, String> {
    let mut costs = basket
        .items
        .iter()
        .map(|item| {
            Decimal::from_str(&item.package_price)
                .map(|price| price * Decimal::from(item.quantity_packages))
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    costs.sort_by(|left, right| right.cmp(left));
    let total: Decimal = costs.iter().copied().sum();
    let top_four: Decimal = costs.iter().take(4).copied().sum();
    let top_ten: Decimal = costs.iter().take(10).copied().sum();
    let hhi: Decimal = costs
        .iter()
        .map(|cost| (*cost / total) * (*cost / total))
        .sum();
    Ok(Concentration {
        top_four_rub: top_four.to_string(),
        top_four_share: ((top_four / total) * Decimal::from(100))
            .round_dp(4)
            .to_string(),
        top_ten_share: ((top_ten / total) * Decimal::from(100))
            .round_dp(4)
            .to_string(),
        hhi: hhi.round_dp(6).to_string(),
        largest_item_sensitivity_10_percent_rub: (costs[0] / Decimal::from(10)).to_string(),
    })
}

fn save_reports(audit: &BasketAudit) -> Result<(), String> {
    fs::create_dir_all("research").map_err(|error| error.to_string())?;
    fs::write(
        "research/phase2_basket_audit.json",
        format!(
            "{}\n",
            serde_json::to_string_pretty(audit).map_err(|error| error.to_string())?
        ),
    )
    .map_err(|error| error.to_string())?;
    let mut markdown = format!(
        "# Аудит корзины Phase 2\n\nСтатус: `{}`. Корзина `{}`, SHA-256 `{}`.\n\nПересчитано без поля total: {} ₽; заявлено {} ₽; отклонение от цели {} ₽. Позиций: {}, уникальных групп: {}.\n\nПодтверждено: {}; с условиями: {}; требует проверки: {}; недействительно: {}.\n\nTop-4: {} ₽ ({}%); top-10: {}%; HHI: {}.\n\n| Группа | Товар | Основание цены | Количество | Акция | Статус | Проблемы |\n|---|---|---|---|---|---|---|\n",
        audit.status,
        audit.basket_id,
        audit.basket_sha256,
        audit.arithmetic_total_rub,
        audit.declared_total_rub,
        audit.deviation_rub,
        audit.item_count,
        audit.unique_groups,
        audit.verified,
        audit.verified_with_conditions,
        audit.requires_review,
        audit.invalid,
        audit.concentration.top_four_rub,
        audit.concentration.top_four_share,
        audit.concentration.top_ten_share,
        audit.concentration.hhi
    );
    for item in &audit.items {
        markdown.push_str(&format!(
            "| {} | {} | `{:?}` | `{:?}` | {} | `{:?}` | {} |\n",
            item.group,
            item.name.replace('|', "\\|"),
            item.price_basis,
            item.quantity_evidence,
            if item.promotion_active {
                "да"
            } else {
                "нет"
            },
            item.status,
            item.issues.join("; ").replace('|', "\\|")
        ));
    }
    markdown.push_str("\n## Доли фактических категорий\n\n| Категория | Стоимость, ₽ | Доля, % |\n|---|---:|---:|\n");
    for category in &audit.category_cost_shares {
        markdown.push_str(&format!(
            "| {} | {} | {} |\n",
            category.key, category.cost_rub, category.share_percent
        ));
    }
    fs::write("research/phase2_basket_audit.md", markdown).map_err(|error| error.to_string())
}

pub fn refresh_saved_report(basket_path: &str, report_path: &str) -> Result<BasketAudit, String> {
    let mut audit = load_report(report_path)?;
    for item in &mut audit.items {
        if item.group == "Кофе" && item.name.to_lowercase().contains("напиток кофейный")
        {
            let message =
                "кофейный напиток не подтверждает соответствие локальной группе чистого кофе";
            if !item.issues.iter().any(|issue| issue == message) {
                item.issues.push(message.to_owned());
            }
            item.suitable_for_fixed_index = false;
            item.status = AuditStatus::RequiresReview;
        }
    }
    audit.verified = audit
        .items
        .iter()
        .filter(|item| item.status == AuditStatus::Verified)
        .count();
    audit.verified_with_conditions = audit
        .items
        .iter()
        .filter(|item| item.status == AuditStatus::VerifiedWithConditions)
        .count();
    audit.requires_review = audit
        .items
        .iter()
        .filter(|item| item.status == AuditStatus::RequiresReview)
        .count();
    audit.invalid = audit
        .items
        .iter()
        .filter(|item| item.status == AuditStatus::Invalid)
        .count();
    audit.status = if audit.requires_review + audit.invalid == 0 {
        "BasketVerified"
    } else {
        "BlockedByBasketAudit"
    }
    .to_owned();
    let basket = basket::load(basket_path)?;
    let (items, categories) = cost_shares(&basket, &audit.items)?;
    audit.item_cost_shares = items;
    audit.category_cost_shares = categories;
    save_reports(&audit)?;
    Ok(audit)
}

fn decimal_json(value: &Value) -> Option<String> {
    (!value.is_null())
        .then(|| Decimal::from_str(&value.to_string()).ok())
        .flatten()
        .map(|value| value.normalize().to_string())
}

pub fn load_report(path: &str) -> Result<BasketAudit, String> {
    let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_weight_package_and_piece_prices() {
        assert_eq!(
            classify_price_basis(Some("кг"), None, "Булочка ВЕС", ""),
            PriceBasis::VariableWeightPrice
        );
        assert_eq!(
            classify_price_basis(Some("шт"), Some("0.15"), "Банан, шт", ""),
            PriceBasis::PiecePrice
        );
        assert_eq!(
            classify_price_basis(Some("шт"), Some("0.9"), "Крупа 900 г", ""),
            PriceBasis::FixedPackagePrice
        );
    }

    #[test]
    fn phase1_basket_arithmetic_is_independent_of_declared_total() {
        let basket: BasketConfig = toml::from_str(include_str!("../config/basket.toml")).unwrap();
        assert_eq!(recalculate_total(&basket).unwrap(), Decimal::from(9932));
        assert_eq!(basket.items.len(), 42);
        assert_eq!(
            basket
                .items
                .iter()
                .map(|item| &item.group)
                .collect::<BTreeSet<_>>()
                .len(),
            42
        );
    }

    #[test]
    fn concentration_matches_known_top_four() {
        let basket: BasketConfig = toml::from_str(include_str!("../config/basket.toml")).unwrap();
        let result = concentration(&basket).unwrap();
        assert_eq!(result.top_four_rub, "4648");
    }
}
