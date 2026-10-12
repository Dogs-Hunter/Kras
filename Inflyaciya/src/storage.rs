use chrono::Utc;
use rusqlite::backup::Backup;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::{fs, path::Path, time::Duration};

use crate::domain::{BasketConfig, ProductCatalog};

const MIGRATION_1: &str = r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS sources (
    id INTEGER PRIMARY KEY,
    source_key TEXT NOT NULL UNIQUE,
    endpoint TEXT NOT NULL,
    access_mode TEXT NOT NULL,
    price_context TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS categories (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);
CREATE TABLE IF NOT EXISTS product_groups (
    id INTEGER PRIMARY KEY,
    category_id INTEGER NOT NULL REFERENCES categories(id),
    name TEXT NOT NULL UNIQUE
);
CREATE TABLE IF NOT EXISTS products (
    id INTEGER PRIMARY KEY,
    source_id INTEGER NOT NULL REFERENCES sources(id),
    source_product_id TEXT NOT NULL UNIQUE,
    mcp_product_id INTEGER NOT NULL,
    xml_id INTEGER,
    group_id INTEGER NOT NULL REFERENCES product_groups(id),
    name TEXT NOT NULL,
    brand TEXT,
    url TEXT NOT NULL,
    first_seen_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS product_packages (
    id INTEGER PRIMARY KEY,
    product_id INTEGER NOT NULL REFERENCES products(id),
    fingerprint TEXT NOT NULL,
    amount TEXT NOT NULL,
    unit TEXT NOT NULL,
    basis TEXT NOT NULL,
    normalized_quantity TEXT NOT NULL,
    normalized_unit TEXT NOT NULL,
    first_seen_at TEXT NOT NULL,
    UNIQUE(product_id, fingerprint)
);
CREATE TABLE IF NOT EXISTS collection_runs (
    id TEXT PRIMARY KEY,
    started_at TEXT NOT NULL,
    completed_at TEXT,
    kind TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    protocol_version TEXT NOT NULL,
    access_mode TEXT NOT NULL,
    price_context TEXT NOT NULL,
    status TEXT NOT NULL,
    error_count INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS price_observations (
    id INTEGER PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES collection_runs(id),
    product_id INTEGER NOT NULL REFERENCES products(id),
    package_id INTEGER NOT NULL REFERENCES product_packages(id),
    observed_at TEXT NOT NULL,
    package_price TEXT NOT NULL,
    old_price TEXT,
    discount_percent TEXT,
    discount_info TEXT,
    normalized_price TEXT NOT NULL,
    normalized_price_unit TEXT NOT NULL,
    price_type TEXT NOT NULL,
    price_context TEXT NOT NULL,
    availability_status TEXT NOT NULL,
    validation_status TEXT NOT NULL,
    UNIQUE(run_id, product_id)
);
CREATE TABLE IF NOT EXISTS baskets (
    id TEXT PRIMARY KEY,
    created_at TEXT NOT NULL,
    approved_at TEXT,
    status TEXT NOT NULL,
    minimum_wage TEXT NOT NULL,
    target TEXT NOT NULL,
    minimum_total TEXT NOT NULL,
    maximum_total TEXT NOT NULL,
    initial_total TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS basket_items (
    basket_id TEXT NOT NULL REFERENCES baskets(id),
    product_id INTEGER NOT NULL REFERENCES products(id),
    group_name TEXT NOT NULL,
    quantity_packages INTEGER NOT NULL,
    initial_package_price TEXT NOT NULL,
    package_fingerprint TEXT NOT NULL,
    PRIMARY KEY(basket_id, group_name)
);
CREATE TABLE IF NOT EXISTS validation_issues (
    id INTEGER PRIMARY KEY,
    run_id TEXT REFERENCES collection_runs(id),
    product_id INTEGER REFERENCES products(id),
    issue_type TEXT NOT NULL,
    message TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_prices_product_time ON price_observations(product_id, observed_at);
CREATE INDEX IF NOT EXISTS idx_prices_run ON price_observations(run_id);
CREATE INDEX IF NOT EXISTS idx_products_group ON products(group_id);
CREATE INDEX IF NOT EXISTS idx_issues_run ON validation_issues(run_id);
"#;

const MIGRATION_2: &str = r#"
CREATE TABLE IF NOT EXISTS methodology_versions (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS basket_versions (
    id TEXT PRIMARY KEY,
    basket_id TEXT NOT NULL REFERENCES baskets(id),
    sha256 TEXT NOT NULL UNIQUE,
    methodology_id TEXT NOT NULL REFERENCES methodology_versions(id),
    source_path TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS basket_audits (
    id TEXT PRIMARY KEY,
    basket_version_id TEXT NOT NULL REFERENCES basket_versions(id),
    created_at TEXT NOT NULL,
    status TEXT NOT NULL,
    report_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS baseline_periods (
    id TEXT PRIMARY KEY,
    basket_version_id TEXT NOT NULL REFERENCES basket_versions(id),
    methodology_id TEXT NOT NULL REFERENCES methodology_versions(id),
    iso_year INTEGER NOT NULL,
    iso_week INTEGER NOT NULL,
    price_context TEXT NOT NULL,
    status TEXT NOT NULL,
    checksum TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(basket_version_id, methodology_id)
);
CREATE TABLE IF NOT EXISTS baseline_items (
    baseline_id TEXT NOT NULL REFERENCES baseline_periods(id),
    product_id INTEGER NOT NULL REFERENCES products(id),
    group_name TEXT NOT NULL,
    category_name TEXT NOT NULL,
    base_quantity TEXT NOT NULL,
    physical_unit TEXT NOT NULL,
    base_price_per_unit TEXT NOT NULL,
    base_package_price TEXT NOT NULL,
    package_fingerprint TEXT NOT NULL,
    price_basis TEXT NOT NULL,
    quantity_evidence TEXT NOT NULL,
    PRIMARY KEY(baseline_id, product_id)
);
CREATE TABLE IF NOT EXISTS weekly_periods (
    id TEXT PRIMARY KEY,
    basket_version_id TEXT NOT NULL REFERENCES basket_versions(id),
    iso_year INTEGER NOT NULL,
    iso_week INTEGER NOT NULL,
    timezone TEXT NOT NULL,
    starts_at_utc TEXT NOT NULL,
    ends_at_utc TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(basket_version_id, iso_year, iso_week)
);
CREATE TABLE IF NOT EXISTS collection_attempts (
    id TEXT PRIMARY KEY,
    weekly_period_id TEXT NOT NULL REFERENCES weekly_periods(id),
    started_at TEXT NOT NULL,
    completed_at TEXT,
    status TEXT NOT NULL,
    success_count INTEGER NOT NULL DEFAULT 0,
    error_count INTEGER NOT NULL DEFAULT 0,
    log_path TEXT
);
CREATE TABLE IF NOT EXISTS weekly_observations (
    id INTEGER PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES collection_attempts(id),
    weekly_period_id TEXT NOT NULL REFERENCES weekly_periods(id),
    product_id INTEGER NOT NULL REFERENCES products(id),
    observed_at TEXT NOT NULL,
    status TEXT NOT NULL,
    published_price TEXT,
    old_price TEXT,
    discount_percent TEXT,
    discount_info TEXT,
    normalized_price TEXT,
    normalized_unit TEXT,
    source_quantity TEXT,
    source_unit TEXT,
    price_basis TEXT NOT NULL,
    package_fingerprint TEXT,
    identity_matches INTEGER NOT NULL,
    raw_json TEXT,
    error_message TEXT,
    UNIQUE(attempt_id, product_id)
);
CREATE TABLE IF NOT EXISTS weekly_snapshots (
    id TEXT PRIMARY KEY,
    weekly_period_id TEXT NOT NULL REFERENCES weekly_periods(id),
    revision INTEGER NOT NULL,
    status TEXT NOT NULL,
    selected_at TEXT NOT NULL,
    published_at TEXT,
    item_count INTEGER NOT NULL,
    success_count INTEGER NOT NULL,
    coverage_count TEXT NOT NULL,
    coverage_base_value TEXT NOT NULL,
    revision_reason TEXT,
    UNIQUE(weekly_period_id, revision)
);
CREATE TABLE IF NOT EXISTS snapshot_items (
    snapshot_id TEXT NOT NULL REFERENCES weekly_snapshots(id),
    product_id INTEGER NOT NULL REFERENCES products(id),
    observation_id INTEGER NOT NULL REFERENCES weekly_observations(id),
    PRIMARY KEY(snapshot_id, product_id)
);
CREATE TABLE IF NOT EXISTS package_change_events (
    id INTEGER PRIMARY KEY,
    observation_id INTEGER NOT NULL REFERENCES weekly_observations(id),
    base_fingerprint TEXT NOT NULL,
    current_fingerprint TEXT NOT NULL,
    comparison_status TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS price_condition_events (
    id INTEGER PRIMARY KEY,
    observation_id INTEGER NOT NULL REFERENCES weekly_observations(id),
    condition_type TEXT NOT NULL,
    condition_text TEXT,
    old_price TEXT,
    discount_percent TEXT,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS index_calculations (
    id TEXT PRIMARY KEY,
    snapshot_id TEXT NOT NULL REFERENCES weekly_snapshots(id),
    baseline_id TEXT NOT NULL REFERENCES baseline_periods(id),
    methodology_id TEXT NOT NULL REFERENCES methodology_versions(id),
    status TEXT NOT NULL,
    fixed_quantity_cost TEXT,
    package_purchase_cost TEXT,
    index_value TEXT,
    change_from_base TEXT,
    weekly_change TEXT,
    comparable_subset_index TEXT,
    calculated_at TEXT NOT NULL,
    diagnostics_json TEXT NOT NULL,
    UNIQUE(snapshot_id, baseline_id, methodology_id)
);
CREATE INDEX IF NOT EXISTS idx_weekly_observations_period_product ON weekly_observations(weekly_period_id, product_id);
CREATE INDEX IF NOT EXISTS idx_weekly_snapshots_period ON weekly_snapshots(weekly_period_id, revision);
CREATE INDEX IF NOT EXISTS idx_attempts_period ON collection_attempts(weekly_period_id, started_at);
CREATE INDEX IF NOT EXISTS idx_baseline_items_product ON baseline_items(product_id);
"#;

const MIGRATION_3: &str = r#"
CREATE TABLE IF NOT EXISTS basket_version_links (
    basket_version_id TEXT PRIMARY KEY REFERENCES basket_versions(id),
    origin_basket_id TEXT NOT NULL,
    origin_sha256 TEXT NOT NULL,
    approved_at TEXT NOT NULL,
    approval_validation_path TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS active_basket_state (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    basket_version_id TEXT NOT NULL REFERENCES basket_versions(id),
    basket_path TEXT NOT NULL,
    activated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS phase21_state (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    status TEXT NOT NULL,
    prepared_basket_path TEXT,
    prepared_sha256 TEXT,
    updated_at TEXT NOT NULL
);
"#;

pub struct Database {
    connection: Connection,
}

#[derive(Debug, Serialize)]
pub struct DatabaseStats {
    pub sources: i64,
    pub categories: i64,
    pub groups: i64,
    pub products: i64,
    pub packages: i64,
    pub observations: i64,
    pub runs: i64,
    pub baskets: i64,
    pub issues: i64,
}

#[derive(Debug, Serialize)]
pub struct HistoryRow {
    pub product_id: String,
    pub name: String,
    pub group_name: String,
    pub observed_at: String,
    pub package_price: String,
    pub normalized_price: String,
    pub normalized_unit: String,
    pub availability_status: String,
    pub run_id: String,
}

impl Database {
    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    pub(crate) fn connection_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }
    pub fn open(path: &str) -> Result<Self, String> {
        if let Some(parent) = Path::new(path).parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("не удалось создать папку базы: {error}"))?;
        }
        let connection = Connection::open(path)
            .map_err(|error| format!("не удалось открыть SQLite {path}: {error}"))?;
        connection
            .execute_batch(
                "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
            )
            .map_err(|error| format!("не удалось настроить SQLite: {error}"))?;
        Ok(Self { connection })
    }

    pub fn migrate(&mut self) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(db_error)?;
        transaction.execute_batch(MIGRATION_1).map_err(db_error)?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES(1, ?1)",
                [Utc::now().to_rfc3339()],
            )
            .map_err(db_error)?;
        transaction.execute_batch(MIGRATION_2).map_err(db_error)?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO methodology_versions(id, name, description, created_at) VALUES('fixed-basket-generic-v1', 'Фиксированная физическая корзина Generic v1', 'Индекс стоимости фиксированной продуктовой корзины по опубликованным ценам общего каталога ВкусВилла', ?1)",
                [Utc::now().to_rfc3339()],
            )
            .map_err(db_error)?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES(2, ?1)",
                [Utc::now().to_rfc3339()],
            )
            .map_err(db_error)?;
        transaction.execute_batch(MIGRATION_3).map_err(db_error)?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES(3, ?1)",
                [Utc::now().to_rfc3339()],
            )
            .map_err(db_error)?;
        transaction.commit().map_err(db_error)
    }

    pub fn backup(source_path: &str, destination_path: &str) -> Result<(), String> {
        if let Some(parent) = Path::new(destination_path).parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let source = Connection::open(source_path).map_err(db_error)?;
        let mut destination = Connection::open(destination_path).map_err(db_error)?;
        let backup = Backup::new(&source, &mut destination).map_err(db_error)?;
        backup
            .run_to_completion(128, Duration::from_millis(20), None)
            .map_err(db_error)?;
        drop(backup);
        let integrity: String = destination
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(db_error)?;
        if integrity != "ok" {
            return Err(format!("резервная копия повреждена: {integrity}"));
        }
        Ok(())
    }

    pub fn foreign_key_check(&self) -> Result<Vec<String>, String> {
        let mut statement = self
            .connection
            .prepare("PRAGMA foreign_key_check")
            .map_err(db_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok(format!(
                    "{}:{}",
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?
                ))
            })
            .map_err(db_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
    }

    pub fn migration_versions(&self) -> Result<Vec<i64>, String> {
        let mut statement = self
            .connection
            .prepare("SELECT version FROM schema_migrations ORDER BY version")
            .map_err(db_error)?;
        let rows = statement
            .query_map([], |row| row.get(0))
            .map_err(db_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
    }

    pub fn save_catalog(&mut self, catalog: &ProductCatalog, kind: &str) -> Result<String, String> {
        let now = Utc::now();
        let run_id = format!("{}-{}", kind, now.timestamp_nanos_opt().unwrap_or_default());
        let transaction = self.connection.transaction().map_err(db_error)?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO sources(source_key, endpoint, access_mode, price_context) VALUES(?1, ?2, 'Anonymous', ?3)",
                params!["vkusvill_official_mcp", catalog.source, catalog.price_context],
            )
            .map_err(db_error)?;
        let source_id: i64 = transaction
            .query_row(
                "SELECT id FROM sources WHERE source_key='vkusvill_official_mcp'",
                [],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        transaction
            .execute(
                "INSERT INTO collection_runs(id, started_at, kind, endpoint, protocol_version, access_mode, price_context, status) VALUES(?1, ?2, ?3, ?4, '2025-03-26', 'Anonymous', ?5, 'running')",
                params![run_id, now.to_rfc3339(), kind, catalog.source, catalog.price_context],
            )
            .map_err(db_error)?;

        for product in &catalog.products {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO categories(name) VALUES(?1)",
                    [&product.category],
                )
                .map_err(db_error)?;
            let category_id: i64 = transaction
                .query_row(
                    "SELECT id FROM categories WHERE name=?1",
                    [&product.category],
                    |row| row.get(0),
                )
                .map_err(db_error)?;
            transaction
                .execute(
                    "INSERT OR IGNORE INTO product_groups(category_id, name) VALUES(?1, ?2)",
                    params![category_id, product.group],
                )
                .map_err(db_error)?;
            let group_id: i64 = transaction
                .query_row(
                    "SELECT id FROM product_groups WHERE name=?1",
                    [&product.group],
                    |row| row.get(0),
                )
                .map_err(db_error)?;
            transaction
                .execute(
                    "INSERT INTO products(source_id, source_product_id, mcp_product_id, xml_id, group_id, name, brand, url, first_seen_at, last_seen_at)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)
                     ON CONFLICT(source_product_id) DO UPDATE SET xml_id=excluded.xml_id, name=excluded.name, brand=excluded.brand, url=excluded.url, last_seen_at=excluded.last_seen_at",
                    params![source_id, product.source_product_id, product.mcp_product_id, product.xml_id, group_id, product.name, product.brand, product.url, product.collected_at.to_rfc3339()],
                )
                .map_err(db_error)?;
            let product_id: i64 = transaction
                .query_row(
                    "SELECT id FROM products WHERE source_product_id=?1",
                    [&product.source_product_id],
                    |row| row.get(0),
                )
                .map_err(db_error)?;
            let fingerprint = product.package_fingerprint();
            let previous: Option<String> = transaction
                .query_row(
                    "SELECT fingerprint FROM product_packages WHERE product_id=?1 ORDER BY id DESC LIMIT 1",
                    [product_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(db_error)?;
            if previous
                .as_deref()
                .is_some_and(|value| value != fingerprint)
            {
                transaction
                    .execute(
                        "INSERT INTO validation_issues(run_id, product_id, issue_type, message, created_at) VALUES(?1, ?2, 'package_changed', ?3, ?4)",
                        params![run_id, product_id, format!("package fingerprint changed from {} to {fingerprint}", previous.unwrap_or_default()), now.to_rfc3339()],
                    )
                    .map_err(db_error)?;
            }
            transaction
                .execute(
                    "INSERT OR IGNORE INTO product_packages(product_id, fingerprint, amount, unit, basis, normalized_quantity, normalized_unit, first_seen_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![product_id, fingerprint, product.package_amount, product.package_unit, product.package_basis, product.normalized_quantity, product.normalized_unit, product.collected_at.to_rfc3339()],
                )
                .map_err(db_error)?;
            let package_id: i64 = transaction
                .query_row(
                    "SELECT id FROM product_packages WHERE product_id=?1 AND fingerprint=?2",
                    params![product_id, fingerprint],
                    |row| row.get(0),
                )
                .map_err(db_error)?;
            transaction
                .execute(
                    "INSERT OR IGNORE INTO price_observations(run_id, product_id, package_id, observed_at, package_price, old_price, discount_percent, discount_info, normalized_price, normalized_price_unit, price_type, price_context, availability_status, validation_status)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                    params![run_id, product_id, package_id, product.collected_at.to_rfc3339(), product.package_price, product.old_price, product.discount_percent, product.discount_info, product.normalized_price, product.normalized_price_unit, if product.old_price.is_some() { "discount" } else { "current" }, product.price_context, product.availability_status, product.validation_status],
                )
                .map_err(db_error)?;
        }
        transaction
            .execute(
                "UPDATE collection_runs SET completed_at=?2, status='completed' WHERE id=?1",
                params![run_id, Utc::now().to_rfc3339()],
            )
            .map_err(db_error)?;
        transaction.commit().map_err(db_error)?;
        Ok(run_id)
    }

    pub fn save_basket(&mut self, basket: &BasketConfig) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(db_error)?;
        let approved = (!basket.approved_at.is_empty()).then_some(basket.approved_at.as_str());
        transaction
            .execute(
                "INSERT INTO baskets(id, created_at, approved_at, status, minimum_wage, target, minimum_total, maximum_total, initial_total)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET approved_at=excluded.approved_at, status=excluded.status",
                params![basket.basket_id, basket.created_at, approved, basket.status, basket.minimum_wage_rub, basket.target_rub, basket.minimum_rub, basket.maximum_rub, basket.total_rub],
            )
            .map_err(db_error)?;
        for item in &basket.items {
            let product_id: i64 = transaction
                .query_row(
                    "SELECT id FROM products WHERE source_product_id=?1",
                    [&item.source_product_id],
                    |row| row.get(0),
                )
                .map_err(db_error)?;
            transaction
                .execute(
                    "INSERT OR IGNORE INTO basket_items(basket_id, product_id, group_name, quantity_packages, initial_package_price, package_fingerprint) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                    params![basket.basket_id, product_id, item.group, item.quantity_packages, item.package_price, item.package_fingerprint],
                )
                .map_err(db_error)?;
        }
        transaction.commit().map_err(db_error)
    }

    pub fn add_issue(
        &self,
        run_id: Option<&str>,
        source_product_id: Option<&str>,
        issue_type: &str,
        message: &str,
    ) -> Result<(), String> {
        let product_id: Option<i64> = source_product_id
            .map(|id| {
                self.connection
                    .query_row(
                        "SELECT id FROM products WHERE source_product_id=?1",
                        [id],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(db_error)
            })
            .transpose()?
            .flatten();
        self.connection
            .execute(
                "INSERT INTO validation_issues(run_id, product_id, issue_type, message, created_at) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![run_id, product_id, issue_type, message, Utc::now().to_rfc3339()],
            )
            .map_err(db_error)?;
        Ok(())
    }

    pub fn history(&self, limit: usize) -> Result<Vec<HistoryRow>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT p.source_product_id, p.name, g.name, o.observed_at, o.package_price, o.normalized_price, o.normalized_price_unit, o.availability_status, o.run_id
                 FROM price_observations o JOIN products p ON p.id=o.product_id JOIN product_groups g ON g.id=p.group_id
                 ORDER BY o.observed_at DESC, p.source_product_id LIMIT ?1",
            )
            .map_err(db_error)?;
        let rows = statement
            .query_map([limit as i64], |row| {
                Ok(HistoryRow {
                    product_id: row.get(0)?,
                    name: row.get(1)?,
                    group_name: row.get(2)?,
                    observed_at: row.get(3)?,
                    package_price: row.get(4)?,
                    normalized_price: row.get(5)?,
                    normalized_unit: row.get(6)?,
                    availability_status: row.get(7)?,
                    run_id: row.get(8)?,
                })
            })
            .map_err(db_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
    }

    pub fn stats(&self) -> Result<DatabaseStats, String> {
        Ok(DatabaseStats {
            sources: self.count("sources")?,
            categories: self.count("categories")?,
            groups: self.count("product_groups")?,
            products: self.count("products")?,
            packages: self.count("product_packages")?,
            observations: self.count("price_observations")?,
            runs: self.count("collection_runs")?,
            baskets: self.count("baskets")?,
            issues: self.count("validation_issues")?,
        })
    }

    fn count(&self, table: &str) -> Result<i64, String> {
        self.connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .map_err(db_error)
    }

    pub fn integrity_check(&self) -> Result<String, String> {
        self.connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(db_error)
    }

    pub fn changed_product_count(&self) -> Result<i64, String> {
        self.connection
            .query_row(
                "SELECT COUNT(*) FROM (
                    SELECT product_id FROM price_observations
                    GROUP BY product_id
                    HAVING MIN(CAST(package_price AS REAL)) <> MAX(CAST(package_price AS REAL))
                )",
                [],
                |row| row.get(0),
            )
            .map_err(db_error)
    }
}

fn db_error(error: rusqlite::Error) -> String {
    format!("ошибка SQLite: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Product, ProductCatalog};
    use chrono::TimeZone;

    fn product(price: &str, package_amount: &str) -> Product {
        Product {
            source_product_id: "vkusvill:1".to_owned(),
            mcp_product_id: 1,
            xml_id: Some(1),
            name: "Тестовый продукт".to_owned(),
            brand: None,
            category: "Категория".to_owned(),
            group: "Группа".to_owned(),
            query: "тест".to_owned(),
            url: "https://example.test/1".to_owned(),
            package_price: price.to_owned(),
            old_price: None,
            discount_percent: None,
            discount_info: None,
            package_amount: package_amount.to_owned(),
            package_unit: "г".to_owned(),
            package_basis: "fixture".to_owned(),
            normalized_quantity: "1".to_owned(),
            normalized_unit: "kg".to_owned(),
            normalized_price: price.to_owned(),
            normalized_price_unit: "RUB/kg".to_owned(),
            collected_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            source: "https://mcp.vkusvill.ru/mcp".to_owned(),
            price_context: "Generic".to_owned(),
            availability_status: "observed".to_owned(),
            validation_status: "CandidateObservation".to_owned(),
        }
    }

    fn memory_database() -> Database {
        let mut database = Database {
            connection: Connection::open_in_memory().unwrap(),
        };
        database
            .connection
            .execute_batch("PRAGMA foreign_keys=ON")
            .unwrap();
        database.migrate().unwrap();
        database
    }

    #[test]
    fn migrations_are_idempotent_and_integrity_is_ok() {
        let mut database = memory_database();
        database.migrate().unwrap();
        assert_eq!(database.integrity_check().unwrap(), "ok");
        assert_eq!(database.stats().unwrap().products, 0);
    }

    #[test]
    fn repeated_runs_preserve_history_without_duplicate_products() {
        let mut database = memory_database();
        let mut catalog = ProductCatalog {
            schema_version: 1,
            generated_at: "2026-01-01T00:00:00Z".to_owned(),
            source: "https://mcp.vkusvill.ru/mcp".to_owned(),
            price_context: "Generic".to_owned(),
            products: vec![product("100", "1000")],
        };
        database.save_catalog(&catalog, "fixture").unwrap();
        catalog.products[0].package_price = "110".to_owned();
        catalog.products[0].normalized_price = "110".to_owned();
        catalog.products[0].collected_at = Utc.with_ymd_and_hms(2026, 1, 8, 0, 0, 0).unwrap();
        database.save_catalog(&catalog, "fixture").unwrap();
        let stats = database.stats().unwrap();
        assert_eq!(stats.products, 1);
        assert_eq!(stats.observations, 2);
        assert_eq!(database.history(10).unwrap().len(), 2);
        assert_eq!(database.changed_product_count().unwrap(), 1);
    }

    #[test]
    fn package_change_is_versioned_and_reported() {
        let mut database = memory_database();
        let mut catalog = ProductCatalog {
            schema_version: 1,
            generated_at: "2026-01-01T00:00:00Z".to_owned(),
            source: "https://mcp.vkusvill.ru/mcp".to_owned(),
            price_context: "Generic".to_owned(),
            products: vec![product("100", "1000")],
        };
        database.save_catalog(&catalog, "fixture").unwrap();
        catalog.products[0].package_amount = "900".to_owned();
        database.save_catalog(&catalog, "fixture").unwrap();
        let stats = database.stats().unwrap();
        assert_eq!(stats.packages, 2);
        assert_eq!(stats.issues, 1);
    }
}
