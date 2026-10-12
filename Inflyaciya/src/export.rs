use chrono::Utc;
use rust_decimal::Decimal;
use serde::Serialize;
use std::{fs, path::Path};

use crate::{
    domain::{BasketConfig, ProductCatalog},
    storage::{Database, HistoryRow},
};

#[derive(Debug, Serialize)]
pub struct ExportReport {
    pub generated_at: String,
    pub price_context: String,
    pub products: usize,
    pub categories: usize,
    pub groups: usize,
    pub normalized_share: String,
    pub basket_items: usize,
    pub basket_total_rub: String,
    pub basket_deviation_rub: String,
    pub observations: i64,
    pub successful_observations: usize,
    pub unsuccessful_issues: i64,
    pub products_with_price_change: i64,
}

pub fn export_all(
    output_directory: &str,
    catalog: &ProductCatalog,
    basket: &BasketConfig,
    database: &Database,
) -> Result<ExportReport, String> {
    fs::create_dir_all(output_directory)
        .map_err(|error| format!("не удалось создать {output_directory}: {error}"))?;
    write_products(&format!("{output_directory}/products.csv"), catalog)?;
    let history = database.history(100_000)?;
    write_history(&format!("{output_directory}/prices.csv"), &history)?;
    write_history(&format!("{output_directory}/history.csv"), &history)?;
    fs::write(
        format!("{output_directory}/basket.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(basket).map_err(|error| error.to_string())?
        ),
    )
    .map_err(|error| error.to_string())?;
    let stats = database.stats()?;
    let categories = catalog
        .products
        .iter()
        .map(|product| &product.category)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let groups = catalog
        .products
        .iter()
        .map(|product| &product.group)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let normalized = catalog
        .products
        .iter()
        .filter(|product| !product.normalized_price.is_empty())
        .count();
    let report = ExportReport {
        generated_at: Utc::now().to_rfc3339(),
        price_context: catalog.price_context.clone(),
        products: catalog.products.len(),
        categories,
        groups,
        normalized_share: if catalog.products.is_empty() {
            "0".to_owned()
        } else {
            (Decimal::from(normalized) / Decimal::from(catalog.products.len()))
                .round_dp(6)
                .normalize()
                .to_string()
        },
        basket_items: basket.items.len(),
        basket_total_rub: basket.total_rub.clone(),
        basket_deviation_rub: (basket
            .total_rub
            .parse::<Decimal>()
            .map_err(|error| error.to_string())?
            - basket
                .target_rub
                .parse::<Decimal>()
                .map_err(|error| error.to_string())?)
        .to_string(),
        observations: stats.observations,
        successful_observations: history.len(),
        unsuccessful_issues: stats.issues,
        products_with_price_change: database.changed_product_count()?,
    };
    fs::write(
        format!("{output_directory}/report.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
        ),
    )
    .map_err(|error| error.to_string())?;
    Ok(report)
}

fn write_products(path: &str, catalog: &ProductCatalog) -> Result<(), String> {
    ensure_parent(path)?;
    let mut writer = csv::Writer::from_path(path).map_err(|error| error.to_string())?;
    for product in &catalog.products {
        writer
            .serialize(product)
            .map_err(|error| error.to_string())?;
    }
    writer.flush().map_err(|error| error.to_string())
}

fn write_history(path: &str, rows: &[HistoryRow]) -> Result<(), String> {
    ensure_parent(path)?;
    let mut writer = csv::Writer::from_path(path).map_err(|error| error.to_string())?;
    for row in rows {
        writer.serialize(row).map_err(|error| error.to_string())?;
    }
    writer.flush().map_err(|error| error.to_string())
}

fn ensure_parent(path: &str) -> Result<(), String> {
    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_serializes_to_json() {
        let report = ExportReport {
            generated_at: "2026-10-10T00:00:00Z".to_owned(),
            price_context: "Generic".to_owned(),
            products: 2,
            categories: 1,
            groups: 2,
            normalized_share: "1".to_owned(),
            basket_items: 2,
            basket_total_rub: "100".to_owned(),
            basket_deviation_rub: "0".to_owned(),
            observations: 2,
            successful_observations: 2,
            unsuccessful_issues: 0,
            products_with_price_change: 0,
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("Generic"));
        assert!(json.contains("basket_total_rub"));
    }

    #[test]
    fn catalog_csv_and_basket_json_are_exportable() {
        let catalog: ProductCatalog =
            toml::from_str(include_str!("../config/products.toml")).unwrap();
        let basket: BasketConfig = toml::from_str(include_str!("../config/basket.toml")).unwrap();
        let path = format!(
            "{}/target/phase1-products-{}.csv",
            env!("CARGO_MANIFEST_DIR"),
            std::process::id()
        );
        write_products(&path, &catalog).unwrap();
        let csv = fs::read_to_string(&path).unwrap();
        assert!(csv.lines().count() > 1);
        assert!(csv.contains("source_product_id"));
        fs::remove_file(path).unwrap();

        let json = serde_json::to_string(&basket).unwrap();
        let restored: BasketConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.basket_id, basket.basket_id);
        assert_eq!(restored.items.len(), basket.items.len());
    }
}
