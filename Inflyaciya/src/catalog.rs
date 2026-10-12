use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeMap, fs, path::Path};

use crate::{
    domain::{GroupSpec, Product, ProductCatalog, Settings},
    mcp::{
        DEFAULT_ENDPOINT,
        client::McpClient,
        products::{normalize_public_item, search_arguments, unwrap_tool_data},
    },
};

#[derive(Debug, Serialize)]
pub struct DiscoverySummary {
    pub generated_at: String,
    pub endpoint: String,
    pub protocol_version: String,
    pub price_context: String,
    pub queries: usize,
    pub categories: usize,
    pub groups: usize,
    pub products: usize,
    pub normalized_products: usize,
    pub rejected_products: usize,
    pub issues: Vec<String>,
}

pub fn load_settings(path: &str) -> Result<Settings, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("не удалось прочитать {path}: {error}"))?;
    toml::from_str(&text).map_err(|error| format!("ошибка TOML в {path}: {error}"))
}

pub fn load_catalog(path: &str) -> Result<ProductCatalog, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("не удалось прочитать {path}: {error}"))?;
    toml::from_str(&text).map_err(|error| format!("ошибка каталога {path}: {error}"))
}

pub fn save_catalog(path: &str, catalog: &ProductCatalog) -> Result<(), String> {
    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("не удалось создать каталог: {error}"))?;
    }
    let text = toml::to_string_pretty(catalog).map_err(|error| error.to_string())?;
    fs::write(path, text).map_err(|error| format!("не удалось записать {path}: {error}"))
}

pub fn discover(settings: &Settings) -> Result<(ProductCatalog, DiscoverySummary), String> {
    let specs = group_specs();
    let mut client = McpClient::with_policy(
        &settings.source.endpoint,
        settings.source.request_timeout_seconds,
        settings.source.max_retries,
        settings.source.backoff_milliseconds,
    )?;
    let initialization = client.initialize()?;
    client.initialized()?;
    let protocol = initialization["protocolVersion"]
        .as_str()
        .unwrap_or("unknown")
        .to_owned();
    let collected_at = Utc::now();
    let mut products = BTreeMap::<String, Product>::new();
    let mut issues = Vec::new();
    let mut rejected = 0usize;

    for spec in &specs {
        let response = client.call_read_only_tool(
            "vkusvill_products_search",
            search_arguments(&spec.query, settings.source.page_limit)?,
        );
        let data = match response.and_then(|value| unwrap_tool_data(&value)) {
            Ok(data) => data,
            Err(error) => {
                issues.push(format!("{} / {}: {error}", spec.group, spec.query));
                continue;
            }
        };
        for mut item in data["items"].as_array().cloned().unwrap_or_default() {
            normalize_public_item(&mut item, &spec.query, collected_at);
            if !is_relevant(&item, spec) {
                rejected += 1;
                issues.push(format!(
                    "{}: исключён нерелевантный результат группы {}",
                    compact_name(&item),
                    spec.group
                ));
                continue;
            }
            match Product::from_normalized_value(&item, spec) {
                Ok(product) => {
                    products
                        .entry(product.source_product_id.clone())
                        .or_insert(product);
                }
                Err(error) => {
                    rejected += 1;
                    issues.push(format!("{}: {error}", compact_name(&item)));
                }
            }
        }
    }
    client.close()?;
    let products = products.into_values().collect::<Vec<_>>();
    let categories = products
        .iter()
        .map(|product| &product.category)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let groups = products
        .iter()
        .map(|product| &product.group)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let catalog = ProductCatalog {
        schema_version: 1,
        generated_at: collected_at.to_rfc3339(),
        source: settings.source.endpoint.clone(),
        price_context: settings.source.price_context.clone(),
        products,
    };
    let summary = DiscoverySummary {
        generated_at: Utc::now().to_rfc3339(),
        endpoint: settings.source.endpoint.clone(),
        protocol_version: protocol,
        price_context: settings.source.price_context.clone(),
        queries: specs.len(),
        categories,
        groups,
        products: catalog.products.len(),
        normalized_products: catalog.products.len(),
        rejected_products: rejected,
        issues,
    };
    Ok((catalog, summary))
}

fn is_relevant(value: &Value, spec: &GroupSpec) -> bool {
    let name = compact_name(value).to_lowercase();
    match spec.group.as_str() {
        "Бананы" => name.contains("банан") && !name.contains("шоколад"),
        "Апельсины" => {
            name.contains("апельсин") && !name.contains("шоколад") && !name.contains("желе")
        }
        "Яблоки" => name.contains("яблок"),
        "Специи" => [
            "приправ",
            "трав",
            "перец",
            "паприк",
            "куркум",
            "кориандр",
            "кориц",
        ]
        .iter()
        .any(|token| name.contains(token)),
        "Сахар" => name.contains("сахар") && !name.contains("ваниль"),
        _ => true,
    }
}

fn compact_name(value: &Value) -> String {
    value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("неизвестный товар")
        .replace("&nbsp;", " ")
}

pub fn group_specs() -> Vec<GroupSpec> {
    let values = [
        ("Молочные продукты", "Молоко", "молоко"),
        ("Молочные продукты", "Кефир", "кефир"),
        ("Молочные продукты", "Сметана", "сметана"),
        ("Молочные продукты", "Творог", "творог"),
        ("Сыры", "Сыр", "сыр"),
        ("Масла", "Сливочное масло", "масло сливочное"),
        ("Масла", "Растительное масло", "масло подсолнечное"),
        ("Мясо", "Говядина", "говядина"),
        ("Мясо", "Свинина", "свинина"),
        ("Птица", "Курица", "курица"),
        ("Птица", "Индейка", "индейка"),
        ("Рыба", "Красная рыба", "лосось"),
        ("Рыба", "Белая рыба", "треска"),
        ("Морепродукты", "Креветки", "креветки"),
        ("Крупы", "Гречка", "гречка"),
        ("Крупы", "Рис", "рис крупа"),
        ("Крупы", "Овсяные хлопья", "овсяные хлопья"),
        ("Макаронные изделия", "Макароны", "макароны"),
        ("Мука", "Пшеничная мука", "мука пшеничная"),
        ("Хлеб", "Хлеб", "хлеб"),
        ("Выпечка", "Выпечка", "булочка выпечка"),
        ("Овощи", "Картофель", "картофель"),
        ("Овощи", "Помидоры", "помидоры"),
        ("Овощи", "Огурцы", "огурцы"),
        ("Овощи", "Морковь", "морковь"),
        ("Фрукты", "Яблоки", "яблоки"),
        ("Фрукты", "Бананы", "бананы"),
        ("Фрукты", "Апельсины", "апельсины"),
        ("Ягоды", "Ягоды", "ягоды"),
        ("Грибы", "Грибы", "грибы"),
        ("Бакалея", "Сахар", "сахар"),
        ("Бакалея", "Соль", "соль"),
        ("Консервы", "Овощные консервы", "горошек консервированный"),
        (
            "Замороженные продукты",
            "Замороженные овощи",
            "овощи замороженные",
        ),
        ("Соусы", "Кетчуп", "кетчуп"),
        ("Приправы", "Специи", "приправа специи"),
        ("Чай", "Чай", "чай"),
        ("Кофе", "Кофе", "кофе"),
        ("Напитки", "Вода", "вода питьевая"),
        ("Напитки", "Сок", "сок"),
        ("Кондитерские изделия", "Шоколад", "шоколад"),
        ("Кондитерские изделия", "Печенье", "печенье"),
    ];
    values
        .into_iter()
        .map(|(category, group, query)| GroupSpec {
            category: category.to_owned(),
            group: group.to_owned(),
            query: query.to_owned(),
            food: true,
        })
        .collect()
}

pub fn default_endpoint() -> &'static str {
    DEFAULT_ENDPOINT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_taxonomy_is_detailed_and_unique() {
        let specs = group_specs();
        let unique = specs
            .iter()
            .map(|spec| &spec.group)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(specs.len() >= 40);
        assert_eq!(specs.len(), unique.len());
        assert!(specs.iter().all(|spec| spec.food));
    }

    #[test]
    fn settings_toml_is_valid() {
        let settings: Settings = toml::from_str(include_str!("../config/settings.toml")).unwrap();
        assert_eq!(settings.source.price_context, "Generic");
        assert!(!settings.policy.allow_ordering_tools);
    }

    #[test]
    fn rejects_processed_food_from_fresh_fruit_groups() {
        let spec = GroupSpec {
            category: "Фрукты".to_owned(),
            group: "Бананы".to_owned(),
            query: "бананы".to_owned(),
            food: true,
        };
        assert!(is_relevant(&serde_json::json!({"name": "Бананы"}), &spec));
        assert!(!is_relevant(
            &serde_json::json!({"name": "Банан в шоколаде"}),
            &spec
        ));
    }
}
