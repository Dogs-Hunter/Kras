use chrono::Utc;
use serde::Serialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs};

use crate::{
    basket,
    catalog::{self, load_catalog, load_settings, save_catalog},
    domain::{GroupSpec, Product, ProductCatalog, Settings},
    export,
    mcp::{
        client::McpClient,
        products::{normalize_public_item, search_arguments, unwrap_tool_data},
    },
    storage::Database,
};

const SETTINGS_PATH: &str = "config/settings.toml";

#[derive(Debug, Serialize)]
struct ReproducibilityRecord {
    product_id: u64,
    name: String,
    query: String,
    search_seen: bool,
    first_details_at: String,
    second_details_at: String,
    same_id: bool,
    same_price: bool,
    same_package: bool,
    same_category: bool,
    personalized: bool,
    price_context: String,
    first_price: Value,
    second_price: Value,
}

#[derive(Debug, Serialize)]
struct ReproducibilityReport {
    generated_at: String,
    endpoint: String,
    protocol_version: String,
    access_mode: String,
    price_context: String,
    checked_products: usize,
    stable_identity: usize,
    stable_price: usize,
    records: Vec<ReproducibilityRecord>,
}

pub fn print_help() {
    println!(
        "Инфляция — Phase 1\n\n\
Использование:\n  inflyaciya <команда>\n\n\
Команды Phase 1:\n  reproducibility       повторить проверку не менее 10 товаров\n  discover              собрать ограниченный широкий каталог через MCP\n  products              показать сохранённые товары\n  basket build          сформировать черновик корзины\n  basket show           показать текущую корзину\n  basket approve        неизменно утвердить состав корзины\n  collect               повторно собрать цены закреплённых товаров\n  history [limit]       показать историю наблюдений\n  export                выгрузить CSV и JSON в output\n  stats                 показать статистику SQLite\n  migrate               применить миграции SQLite\n\n\
Исследовательские команды Phase 0/0.5/0.6 сохранены: mcp ..., --phase05."
    );
}

pub fn is_phase1_command(value: &str) -> bool {
    matches!(
        value,
        "reproducibility"
            | "discover"
            | "products"
            | "basket"
            | "collect"
            | "history"
            | "export"
            | "stats"
            | "migrate"
    )
}

pub fn run(args: &[String]) -> Result<(), String> {
    let command = args
        .first()
        .map(String::as_str)
        .ok_or("не указана команда")?;
    let settings = load_settings(SETTINGS_PATH)?;
    validate_settings(&settings)?;
    match command {
        "discover" => discover_command(&settings),
        "reproducibility" => reproducibility_command(&settings),
        "products" => products_command(&settings),
        "basket" => basket_command(&settings, &args[1..]),
        "collect" => collect_command(&settings),
        "history" => history_command(&settings, args.get(1)),
        "export" => export_command(&settings),
        "stats" => stats_command(&settings),
        "migrate" => migrate_command(&settings),
        other => Err(format!("неизвестная команда Phase 1: {other}")),
    }
}

fn database(settings: &Settings) -> Result<Database, String> {
    let mut database = Database::open(&settings.collection.database_path)?;
    database.migrate()?;
    Ok(database)
}

fn validate_settings(settings: &Settings) -> Result<(), String> {
    if settings.source.id != "vkusvill_official_mcp" {
        return Err("неизвестный идентификатор источника".to_owned());
    }
    if settings.source.access_mode != "Anonymous" || settings.source.price_context != "Generic" {
        return Err("Phase 1 допускает только Anonymous/Generic".to_owned());
    }
    if settings.policy.allow_ordering_tools {
        return Err("операции заказов должны быть запрещены".to_owned());
    }
    if !settings.policy.one_product_per_group
        || !settings.policy.food_only_main_basket
        || !settings.policy.require_normalized_kg_or_l
    {
        return Err("настройки основной корзины ослабляют обязательные ограничения".to_owned());
    }
    if settings.policy.allow_automatic_replacement_after_approval {
        return Err("автоматическая замена утверждённых товаров запрещена".to_owned());
    }
    if settings.source.endpoint != catalog::default_endpoint() {
        return Err("настроен неофициальный MCP endpoint".to_owned());
    }
    let _ = settings.budget.calculate()?;
    Ok(())
}

fn discover_command(settings: &Settings) -> Result<(), String> {
    let (catalog, summary) = catalog::discover(settings)?;
    save_catalog(&settings.collection.products_path, &catalog)?;
    let mut database = database(settings)?;
    let run_id = database.save_catalog(&catalog, "discover")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"run_id": run_id, "summary": summary}))
            .map_err(|error| error.to_string())?
    );
    Ok(())
}

fn products_command(settings: &Settings) -> Result<(), String> {
    let catalog = load_catalog(&settings.collection.products_path)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&catalog).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn basket_command(settings: &Settings, args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("build") => {
            if let Ok(existing) = basket::load(&settings.collection.basket_path)
                && existing.status == "approved"
            {
                return Err(format!(
                    "корзина {} уже утверждена; создайте новую версию явно, не заменяйте состав",
                    existing.basket_id
                ));
            }
            let catalog = load_catalog(&settings.collection.products_path)?;
            let budget = settings.budget.calculate()?;
            let candidate = basket::optimize(&catalog, &budget)?;
            basket::save(&settings.collection.basket_path, &candidate)?;
            let mut database = database(settings)?;
            database.save_basket(&candidate)?;
            print_json(&candidate)
        }
        Some("show") => print_json(&basket::load(&settings.collection.basket_path)?),
        Some("approve") => {
            let approved = basket::approve(&settings.collection.basket_path)?;
            let mut database = database(settings)?;
            database.save_basket(&approved)?;
            print_json(&approved)
        }
        _ => Err("используйте basket build, basket show или basket approve".to_owned()),
    }
}

fn collect_command(settings: &Settings) -> Result<(), String> {
    let basket = basket::load(&settings.collection.basket_path)?;
    if basket.status != "approved" {
        return Err("сначала утвердите корзину командой basket approve".to_owned());
    }
    let catalog = load_catalog(&settings.collection.products_path)?;
    let by_id = catalog
        .products
        .iter()
        .map(|product| (product.source_product_id.as_str(), product))
        .collect::<BTreeMap<_, _>>();
    let mut client = initialized_client(settings)?.0;
    let mut collected = Vec::new();
    let mut failures = Vec::new();
    for item in &basket.items {
        let Some(original) = by_id.get(item.source_product_id.as_str()) else {
            failures.push((
                item.source_product_id.clone(),
                "товар отсутствует в локальном каталоге".to_owned(),
            ));
            continue;
        };
        let response = client
            .call_read_only_tool(
                "vkusvill_product_details",
                json!({"id": item.mcp_product_id}),
            )
            .and_then(|value| unwrap_tool_data(&value));
        match response {
            Ok(mut value) => {
                if value.get("id").and_then(Value::as_u64) != Some(item.mcp_product_id) {
                    failures.push((
                        item.source_product_id.clone(),
                        "источник вернул другой product ID".to_owned(),
                    ));
                    continue;
                }
                normalize_public_item(&mut value, &original.query, Utc::now());
                let spec = GroupSpec {
                    category: original.category.clone(),
                    group: original.group.clone(),
                    query: original.query.clone(),
                    food: true,
                };
                match Product::from_normalized_value(&value, &spec) {
                    Ok(mut product) => {
                        product.availability_status = "details_returned_no_stock_field".to_owned();
                        if product.package_fingerprint() != item.package_fingerprint {
                            failures.push((
                                item.source_product_id.clone(),
                                format!(
                                    "изменилась упаковка: {} -> {}",
                                    item.package_fingerprint,
                                    product.package_fingerprint()
                                ),
                            ));
                        }
                        collected.push(product);
                    }
                    Err(error) => failures.push((item.source_product_id.clone(), error)),
                }
            }
            Err(error) => failures.push((
                item.source_product_id.clone(),
                classify_collection_error(&error),
            )),
        }
    }
    client.close()?;
    if collected.is_empty() {
        return Err("не получено ни одной новой цены; старые цены не записаны повторно".to_owned());
    }
    let run_catalog = ProductCatalog {
        schema_version: 1,
        generated_at: Utc::now().to_rfc3339(),
        source: settings.source.endpoint.clone(),
        price_context: "Generic".to_owned(),
        products: collected,
    };
    let mut database = database(settings)?;
    let run_id = database.save_catalog(&run_catalog, "collect")?;
    for (product_id, message) in &failures {
        database.add_issue(
            Some(&run_id),
            Some(product_id),
            issue_type(message),
            message,
        )?;
    }
    print_json(&json!({
        "run_id": run_id,
        "successful": run_catalog.products.len(),
        "failed": failures.len(),
        "failures": failures,
        "basket_unchanged": true
    }))
}

fn classify_collection_error(error: &str) -> String {
    if error.contains("Authentication") {
        format!("ошибка авторизации: {error}")
    } else if error.contains("RateLimit") || error.contains("429") {
        format!("ограничение запросов: {error}")
    } else if error.contains("Transport") || error.contains("timeout") {
        format!("источник недоступен или таймаут: {error}")
    } else if error.contains("InvalidData") || error.contains("schema") {
        format!("изменилась структура ответа: {error}")
    } else {
        format!("цена не получена: {error}")
    }
}

fn issue_type(message: &str) -> &'static str {
    if message.contains("упаков") {
        "package_changed"
    } else if message.contains("таймаут") || message.contains("недоступен") {
        "source_unavailable"
    } else if message.contains("структур") {
        "schema_changed"
    } else if message.contains("ID") {
        "product_identity_changed"
    } else {
        "price_missing"
    }
}

fn reproducibility_command(settings: &Settings) -> Result<(), String> {
    let catalog = load_catalog(&settings.collection.products_path)?;
    if catalog.products.len() < 10 {
        return Err(
            "для проверки воспроизводимости нужно сначала discover и не менее 10 товаров"
                .to_owned(),
        );
    }
    let selected = catalog.products.iter().take(10).collect::<Vec<_>>();
    let (mut first_client, initialization) = initialized_client(settings)?;
    let protocol = initialization["protocolVersion"]
        .as_str()
        .unwrap_or("unknown")
        .to_owned();
    let mut records = Vec::new();
    for product in selected {
        let search = unwrap_tool_data(&first_client.call_read_only_tool(
            "vkusvill_products_search",
            search_arguments(&product.query, 10)?,
        )?)?;
        let search_seen = search["items"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|item| item.get("id").and_then(Value::as_u64) == Some(product.mcp_product_id));
        let first_at = Utc::now();
        let first = unwrap_tool_data(&first_client.call_read_only_tool(
            "vkusvill_product_details",
            json!({"id": product.mcp_product_id}),
        )?)?;
        let (mut second_client, _) = initialized_client(settings)?;
        let second_at = Utc::now();
        let second = unwrap_tool_data(&second_client.call_read_only_tool(
            "vkusvill_product_details",
            json!({"id": product.mcp_product_id}),
        )?)?;
        second_client.close()?;
        records.push(ReproducibilityRecord {
            product_id: product.mcp_product_id,
            name: product.name.clone(),
            query: product.query.clone(),
            search_seen,
            first_details_at: first_at.to_rfc3339(),
            second_details_at: second_at.to_rfc3339(),
            same_id: first["id"] == second["id"],
            same_price: first["price"] == second["price"],
            same_package: first["weight"] == second["weight"] && first["unit"] == second["unit"],
            same_category: first["category"] == second["category"],
            personalized: false,
            price_context: "Generic".to_owned(),
            first_price: first["price"].clone(),
            second_price: second["price"].clone(),
        });
    }
    first_client.close()?;
    let report = ReproducibilityReport {
        generated_at: Utc::now().to_rfc3339(),
        endpoint: settings.source.endpoint.clone(),
        protocol_version: protocol,
        access_mode: "Anonymous".to_owned(),
        price_context: "Generic".to_owned(),
        checked_products: records.len(),
        stable_identity: records
            .iter()
            .filter(|record| record.same_id && record.same_package && record.same_category)
            .count(),
        stable_price: records.iter().filter(|record| record.same_price).count(),
        records,
    };
    fs::create_dir_all("research").map_err(|error| error.to_string())?;
    fs::write(
        "research/phase1_reproducibility.json",
        format!(
            "{}\n",
            serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
        ),
    )
    .map_err(|error| error.to_string())?;
    print_json(&report)
}

fn initialized_client(settings: &Settings) -> Result<(McpClient, Value), String> {
    let mut client = McpClient::with_policy(
        &settings.source.endpoint,
        settings.source.request_timeout_seconds,
        settings.source.max_retries,
        settings.source.backoff_milliseconds,
    )?;
    let initialization = client.initialize()?;
    client.initialized()?;
    Ok((client, initialization))
}

fn history_command(settings: &Settings, limit: Option<&String>) -> Result<(), String> {
    let limit = limit
        .map(|value| value.parse::<usize>().map_err(|error| error.to_string()))
        .transpose()?
        .unwrap_or(100);
    print_json(&database(settings)?.history(limit)?)
}

fn stats_command(settings: &Settings) -> Result<(), String> {
    let database = database(settings)?;
    let stats = database.stats()?;
    print_json(&json!({
        "database": stats,
        "integrity": database.integrity_check()?,
        "collection_frequency_days": settings.collection.frequency_days,
        "source_timeout_seconds": settings.source.request_timeout_seconds,
        "max_retries": settings.source.max_retries,
        "backoff_milliseconds": settings.source.backoff_milliseconds,
        "budget_region": settings.budget.region,
        "budget_year": settings.budget.year,
        "normative_source": settings.budget.normative_source
    }))
}

fn migrate_command(settings: &Settings) -> Result<(), String> {
    let database = database(settings)?;
    print_json(&json!({
        "result": "ok",
        "integrity": database.integrity_check()?,
        "foreign_key_check": database.foreign_key_check()?,
        "migration_versions": database.migration_versions()?
    }))
}

fn export_command(settings: &Settings) -> Result<(), String> {
    let catalog = load_catalog(&settings.collection.products_path)?;
    let basket = basket::load(&settings.collection.basket_path)?;
    let database = database(settings)?;
    let report = export::export_all(
        &settings.collection.output_directory,
        &catalog,
        &basket,
        &database,
    )?;
    print_json(&report)
}

fn print_json<T: Serialize>(value: &T) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_collection_failures() {
        assert!(classify_collection_error("HTTP 429").contains("ограничение"));
        assert!(classify_collection_error("timeout").contains("таймаут"));
        assert_eq!(issue_type("изменилась упаковка"), "package_changed");
    }

    #[test]
    fn phase1_commands_are_recognized() {
        assert!(is_phase1_command("discover"));
        assert!(is_phase1_command("basket"));
        assert!(!is_phase1_command("mcp"));
    }
}
