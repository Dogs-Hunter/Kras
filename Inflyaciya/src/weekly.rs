use chrono::{DateTime, Datelike, Duration, NaiveTime, TimeZone, Utc};
use chrono_tz::Europe::Moscow;
use fs2::FileExt;
use rusqlite::{OptionalExtension, params};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
    str::FromStr,
    time::Instant,
};

use crate::{
    audit::{self, AuditStatus, BasketAudit, PriceBasis},
    basket,
    domain::Settings,
    index::{self, IndexItem},
    mcp::{
        client::McpClient,
        products::{normalize_public_item, unwrap_tool_data},
    },
    storage::Database,
};

const METHODOLOGY_ID: &str = "fixed-basket-generic-v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WeekKey {
    pub iso_year: i32,
    pub iso_week: u32,
    pub starts_at_utc: String,
    pub ends_at_utc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeeklyObservation {
    pub source_product_id: String,
    pub mcp_product_id: u64,
    pub group: String,
    pub name: String,
    pub observed_at: String,
    pub status: String,
    pub published_price: Option<String>,
    pub old_price: Option<String>,
    pub discount_percent: Option<String>,
    pub discount_info: Option<String>,
    pub normalized_price: Option<String>,
    pub normalized_unit: Option<String>,
    pub source_quantity: Option<String>,
    pub source_unit: Option<String>,
    pub price_basis: PriceBasis,
    pub package_fingerprint: Option<String>,
    pub identity_matches: bool,
    pub error: Option<String>,
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeeklyRunReport {
    pub generated_at: String,
    pub basket_id: String,
    pub basket_sha256: String,
    pub period_id: String,
    pub iso_year: i32,
    pub iso_week: u32,
    pub attempt_id: String,
    pub snapshot_id: String,
    pub snapshot_status: String,
    pub price_context: String,
    pub total_items: usize,
    pub successful: usize,
    pub errors: usize,
    pub coverage_count_percent: String,
    pub coverage_base_value_percent: String,
    pub full_index: Option<String>,
    pub comparable_subset_index: Option<String>,
    pub warnings: Vec<String>,
    pub observations: Vec<WeeklyObservation>,
}

#[derive(Serialize)]
struct WeeklyCsvRow<'a> {
    source_product_id: &'a str,
    mcp_product_id: u64,
    group: &'a str,
    name: &'a str,
    observed_at: &'a str,
    status: &'a str,
    published_price: Option<&'a str>,
    old_price: Option<&'a str>,
    discount_percent: Option<&'a str>,
    discount_info: Option<&'a str>,
    normalized_price: Option<&'a str>,
    normalized_unit: Option<&'a str>,
    price_basis: String,
    package_fingerprint: Option<&'a str>,
    identity_matches: bool,
    error: Option<&'a str>,
}

impl<'a> From<&'a WeeklyObservation> for WeeklyCsvRow<'a> {
    fn from(value: &'a WeeklyObservation) -> Self {
        Self {
            source_product_id: &value.source_product_id,
            mcp_product_id: value.mcp_product_id,
            group: &value.group,
            name: &value.name,
            observed_at: &value.observed_at,
            status: &value.status,
            published_price: value.published_price.as_deref(),
            old_price: value.old_price.as_deref(),
            discount_percent: value.discount_percent.as_deref(),
            discount_info: value.discount_info.as_deref(),
            normalized_price: value.normalized_price.as_deref(),
            normalized_unit: value.normalized_unit.as_deref(),
            price_basis: format!("{:?}", value.price_basis),
            package_fingerprint: value.package_fingerprint.as_deref(),
            identity_matches: value.identity_matches,
            error: value.error.as_deref(),
        }
    }
}

pub struct CollectionLock {
    file: File,
}

impl CollectionLock {
    pub fn acquire(path: &str) -> Result<Self, String> {
        if let Some(parent) = Path::new(path).parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| error.to_string())?;
        file.try_lock_exclusive()
            .map_err(|_| "другой процесс уже выполняет недельный сбор".to_owned())?;
        Ok(Self { file })
    }
}

impl Drop for CollectionLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub fn week_for(now: DateTime<Utc>) -> WeekKey {
    let local = now.with_timezone(&Moscow);
    let iso = local.iso_week();
    let days_from_monday = i64::from(local.weekday().num_days_from_monday());
    let monday = local.date_naive() - Duration::days(days_from_monday);
    let start_local = Moscow
        .from_local_datetime(&monday.and_time(NaiveTime::MIN))
        .single()
        .expect("Moscow has no DST ambiguity");
    let end_local = start_local + Duration::days(7);
    WeekKey {
        iso_year: iso.year(),
        iso_week: iso.week(),
        starts_at_utc: start_local.with_timezone(&Utc).to_rfc3339(),
        ends_at_utc: end_local.with_timezone(&Utc).to_rfc3339(),
    }
}

pub fn register_basket_and_audit(
    database: &mut Database,
    settings: &Settings,
    audit: &BasketAudit,
) -> Result<String, String> {
    let basket = basket::load(&settings.collection.basket_path)?;
    database.save_basket(&basket)?;
    let version_id = format!("{}-{}", basket.basket_id, &audit.basket_sha256[..12]);
    let transaction = database.connection_mut().transaction().map_err(db_error)?;
    transaction.execute(
        "INSERT OR IGNORE INTO basket_versions(id, basket_id, sha256, methodology_id, source_path, status, created_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![version_id, basket.basket_id, audit.basket_sha256, METHODOLOGY_ID, settings.collection.basket_path, audit.status, Utc::now().to_rfc3339()],
    ).map_err(db_error)?;
    transaction.execute(
        "INSERT OR REPLACE INTO basket_audits(id, basket_version_id, created_at, status, report_json) VALUES(?1, ?2, ?3, ?4, ?5)",
        params![format!("audit-{}", audit.generated_at), version_id, audit.generated_at, audit.status, serde_json::to_string(audit).map_err(|error| error.to_string())?],
    ).map_err(db_error)?;
    transaction.commit().map_err(db_error)?;
    Ok(version_id)
}

pub fn create_baseline(
    database: &mut Database,
    settings: &Settings,
    audit: &BasketAudit,
) -> Result<Value, String> {
    let version_id = register_basket_and_audit(database, settings, audit)?;
    let week = week_for(Utc::now());
    let baseline_id = format!(
        "baseline-{}-{}-W{:02}",
        &audit.basket_sha256[..12],
        week.iso_year,
        week.iso_week
    );
    let status = if audit.status == "BasketVerified" {
        "BaselineReady"
    } else {
        "BlockedByBasketAudit"
    };
    database.connection().execute(
        "INSERT OR IGNORE INTO baseline_periods(id, basket_version_id, methodology_id, iso_year, iso_week, price_context, status, checksum, created_at) VALUES(?1, ?2, ?3, ?4, ?5, 'Generic', ?6, ?7, ?8)",
        params![baseline_id, version_id, METHODOLOGY_ID, week.iso_year, week.iso_week, status, audit.basket_sha256, Utc::now().to_rfc3339()],
    ).map_err(db_error)?;
    if status == "BaselineReady" {
        let transaction = database.connection_mut().transaction().map_err(db_error)?;
        for item in &audit.items {
            let product_id: i64 = transaction
                .query_row(
                    "SELECT id FROM products WHERE source_product_id=?1",
                    [&item.source_product_id],
                    |row| row.get(0),
                )
                .map_err(db_error)?;
            let quantity = item.source_weight.clone().unwrap_or_else(|| "1".to_owned());
            let unit = item
                .source_weight_unit
                .clone()
                .unwrap_or_else(|| item.sale_unit.clone().unwrap_or_default());
            transaction.execute(
                "INSERT OR IGNORE INTO baseline_items(baseline_id, product_id, group_name, category_name, base_quantity, physical_unit, base_price_per_unit, base_package_price, package_fingerprint, price_basis, quantity_evidence) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![baseline_id, product_id, item.group, item.category, quantity, unit, item.normalized_price, item.original_price, "audit", format!("{:?}", item.price_basis), format!("{:?}", item.quantity_evidence)],
            ).map_err(db_error)?;
        }
        transaction.commit().map_err(db_error)?;
    }
    Ok(
        json!({"baseline_id": baseline_id, "status": status, "basket_version_id": version_id, "index_published": false}),
    )
}

#[allow(dead_code)]
pub fn baseline_status(database: &Database) -> Result<Value, String> {
    database.connection().query_row(
        "SELECT id, status, iso_year, iso_week, checksum FROM baseline_periods ORDER BY created_at DESC LIMIT 1",
        [],
        |row| Ok(json!({"baseline_id": row.get::<_, String>(0)?, "status": row.get::<_, String>(1)?, "iso_year": row.get::<_, i32>(2)?, "iso_week": row.get::<_, u32>(3)?, "checksum": row.get::<_, String>(4)?})),
    ).optional().map_err(db_error).map(|value| value.unwrap_or_else(|| json!({"status": "BaselineIncomplete"})))
}

pub fn collect(
    settings: &Settings,
    database: &mut Database,
    audit: &BasketAudit,
    revision_reason: Option<&str>,
) -> Result<WeeklyRunReport, String> {
    let _lock = CollectionLock::acquire("data/weekly-collection.lock")?;
    let started = Instant::now();
    let log_path = format!("{}/weekly.jsonl", settings.collection.logs_directory);
    let basket = basket::load(&settings.collection.basket_path)?;
    let current_hash = audit::sha256_file(&settings.collection.basket_path)?;
    if current_hash != audit.basket_sha256 {
        return Err("контрольная сумма утверждённой корзины изменилась после аудита".to_owned());
    }
    let version_id = register_basket_and_audit(database, settings, audit)?;
    let week = week_for(Utc::now());
    let period_id = format!("{}-{}-W{:02}", version_id, week.iso_year, week.iso_week);
    database.connection().execute(
        "INSERT OR IGNORE INTO weekly_periods(id, basket_version_id, iso_year, iso_week, timezone, starts_at_utc, ends_at_utc, status, created_at) VALUES(?1, ?2, ?3, ?4, 'Europe/Moscow', ?5, ?6, 'open', ?7)",
        params![period_id, version_id, week.iso_year, week.iso_week, week.starts_at_utc, week.ends_at_utc, Utc::now().to_rfc3339()],
    ).map_err(db_error)?;
    let existing_complete: Option<String> = database.connection().query_row(
        "SELECT id FROM weekly_snapshots WHERE weekly_period_id=?1 AND success_count=item_count ORDER BY revision DESC LIMIT 1",
        [&period_id], |row| row.get(0),
    ).optional().map_err(db_error)?;
    if let Some(snapshot_id) = existing_complete
        && revision_reason.is_none()
    {
        return load_weekly_report("output/weekly_report.json").map_err(|_| format!("период уже полностью собран в снимке {snapshot_id}; для исправления нужна явная ревизия"));
    }
    rotate_log(&log_path, 5_000_000)?;
    log_event(
        &log_path,
        json!({"event":"start", "basket_id":basket.basket_id, "period_id":period_id}),
    )?;
    let attempt_id = format!(
        "attempt-{}",
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    database.connection().execute(
        "INSERT INTO collection_attempts(id, weekly_period_id, started_at, status, log_path) VALUES(?1, ?2, ?3, 'running', ?4)",
        params![attempt_id, period_id, Utc::now().to_rfc3339(), log_path],
    ).map_err(db_error)?;
    let mut client = McpClient::with_policy(
        &settings.source.endpoint,
        settings.source.request_timeout_seconds,
        settings.source.max_retries,
        settings.source.backoff_milliseconds,
    )?;
    client.initialize()?;
    client.initialized()?;
    let audit_by_id = audit
        .items
        .iter()
        .map(|item| (item.mcp_product_id, item))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut observations = Vec::new();
    for item in &basket.items {
        let observed_at = Utc::now();
        let response = client
            .call_read_only_tool(
                "vkusvill_product_details",
                json!({"id": item.mcp_product_id}),
            )
            .and_then(|value| unwrap_tool_data(&value));
        let observation = match response {
            Ok(mut details) => {
                let name = details["name"]
                    .as_str()
                    .unwrap_or_default()
                    .replace("&nbsp;", " ");
                let identity =
                    details["id"].as_u64() == Some(item.mcp_product_id) && name == item.name;
                normalize_public_item(&mut details, "weekly", observed_at);
                let unit = details["unit"].as_str().map(str::to_owned);
                let weight = decimal_json(&details["weight"]["value"]);
                let description = details["description"]
                    .as_str()
                    .unwrap_or_default()
                    .to_lowercase();
                let basis = audit::classify_price_basis(
                    unit.as_deref(),
                    weight.as_deref(),
                    &name,
                    &description,
                );
                let fingerprint = details["package"].as_object().map(|package| {
                    format!(
                        "{}|{}|{}|{}",
                        package
                            .get("amount")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                        package
                            .get("unit")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                        package
                            .get("normalized_quantity")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                        package
                            .get("normalized_unit")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                    )
                });
                let base_audit = audit_by_id.get(&item.mcp_product_id).copied();
                let package_changed = fingerprint
                    .as_deref()
                    .is_some_and(|value| value != item.package_fingerprint);
                let status = if !identity {
                    "IdentityConflict"
                } else if matches!(basis, PriceBasis::Unknown) {
                    "UnverifiedPriceBasis"
                } else if package_changed {
                    "PackageChanged"
                } else if base_audit.is_some_and(|value| value.status != AuditStatus::Verified) {
                    "ObservedWithConditions"
                } else {
                    "Observed"
                };
                WeeklyObservation {
                    source_product_id: item.source_product_id.clone(),
                    mcp_product_id: item.mcp_product_id,
                    group: item.group.clone(),
                    name,
                    observed_at: observed_at.to_rfc3339(),
                    status: status.to_owned(),
                    published_price: decimal_json(&details["price"]["current"]),
                    old_price: decimal_json(&details["price"]["old"]),
                    discount_percent: decimal_json(&details["price"]["discount_percent"]),
                    discount_info: details["price"]["discount_info"]
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| base_audit.and_then(|value| value.discount_info.clone())),
                    normalized_price: details["normalized_price"]["value"]
                        .as_str()
                        .map(str::to_owned),
                    normalized_unit: details["normalized_price"]["unit"]
                        .as_str()
                        .map(str::to_owned),
                    source_quantity: details["package"]["normalized_quantity"]
                        .as_str()
                        .map(str::to_owned),
                    source_unit: details["package"]["normalized_unit"]
                        .as_str()
                        .map(str::to_owned),
                    price_basis: basis,
                    package_fingerprint: fingerprint,
                    identity_matches: identity,
                    error: None,
                    raw: details,
                }
            }
            Err(error) => WeeklyObservation {
                source_product_id: item.source_product_id.clone(),
                mcp_product_id: item.mcp_product_id,
                group: item.group.clone(),
                name: item.name.clone(),
                observed_at: observed_at.to_rfc3339(),
                status: classify_error(&error).to_owned(),
                published_price: None,
                old_price: None,
                discount_percent: None,
                discount_info: None,
                normalized_price: None,
                normalized_unit: None,
                source_quantity: None,
                source_unit: None,
                price_basis: PriceBasis::Unknown,
                package_fingerprint: None,
                identity_matches: false,
                error: Some(error),
                raw: Value::Null,
            },
        };
        observations.push(observation);
    }
    client.close()?;
    let success = observations
        .iter()
        .filter(|value| {
            matches!(
                value.status.as_str(),
                "Observed" | "ObservedWithConditions" | "PackageChanged"
            )
        })
        .count();
    let errors = observations.len() - success;
    let revision: i64 = database
        .connection()
        .query_row(
            "SELECT COALESCE(MAX(revision), 0)+1 FROM weekly_snapshots WHERE weekly_period_id=?1",
            [&period_id],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    let snapshot_id = format!("snapshot-{}-r{}", period_id, revision);
    let snapshot_status = if success == basket.items.len() && audit.status == "BasketVerified" {
        "Published"
    } else if success == basket.items.len() {
        "CollectedNotPublishable"
    } else {
        "Incomplete"
    };
    let transaction = database.connection_mut().transaction().map_err(db_error)?;
    for observation in &observations {
        let product_id: i64 = transaction
            .query_row(
                "SELECT id FROM products WHERE source_product_id=?1",
                [&observation.source_product_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        transaction.execute(
            "INSERT INTO weekly_observations(attempt_id, weekly_period_id, product_id, observed_at, status, published_price, old_price, discount_percent, discount_info, normalized_price, normalized_unit, source_quantity, source_unit, price_basis, package_fingerprint, identity_matches, raw_json, error_message) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
            params![attempt_id, period_id, product_id, observation.observed_at, observation.status, observation.published_price, observation.old_price, observation.discount_percent, observation.discount_info, observation.normalized_price, observation.normalized_unit, observation.source_quantity, observation.source_unit, format!("{:?}", observation.price_basis), observation.package_fingerprint, observation.identity_matches, serde_json::to_string(&observation.raw).map_err(|error| error.to_string())?, observation.error],
        ).map_err(db_error)?;
        let observation_id = transaction.last_insert_rowid();
        if observation.status == "PackageChanged" {
            let base = basket
                .items
                .iter()
                .find(|item| item.source_product_id == observation.source_product_id)
                .map(|item| item.package_fingerprint.as_str())
                .unwrap_or_default();
            transaction.execute("INSERT INTO package_change_events(observation_id, base_fingerprint, current_fingerprint, comparison_status, created_at) VALUES(?1,?2,?3,'RequiresReview',?4)", params![observation_id, base, observation.package_fingerprint, Utc::now().to_rfc3339()]).map_err(db_error)?;
        }
        if observation.old_price.is_some() || observation.discount_info.is_some() {
            transaction.execute("INSERT INTO price_condition_events(observation_id, condition_type, condition_text, old_price, discount_percent, created_at) VALUES(?1,'PublishedPromotion',?2,?3,?4,?5)", params![observation_id, observation.discount_info, observation.old_price, observation.discount_percent, Utc::now().to_rfc3339()]).map_err(db_error)?;
        }
    }
    transaction.execute("INSERT INTO weekly_snapshots(id, weekly_period_id, revision, status, selected_at, published_at, item_count, success_count, coverage_count, coverage_base_value, revision_reason) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,?10)", params![snapshot_id, period_id, revision, snapshot_status, Utc::now().to_rfc3339(), (snapshot_status == "Published").then(|| Utc::now().to_rfc3339()), basket.items.len(), success, percent(success, basket.items.len()), revision_reason]).map_err(db_error)?;
    transaction.execute("INSERT INTO snapshot_items(snapshot_id, product_id, observation_id) SELECT ?1, product_id, id FROM weekly_observations WHERE attempt_id=?2", params![snapshot_id, attempt_id]).map_err(db_error)?;
    transaction.execute("UPDATE collection_attempts SET completed_at=?2, status=?3, success_count=?4, error_count=?5 WHERE id=?1", params![attempt_id, Utc::now().to_rfc3339(), if errors == 0 { "completed" } else { "partial" }, success, errors]).map_err(db_error)?;
    transaction
        .execute(
            "UPDATE weekly_periods SET status=?2 WHERE id=?1",
            params![period_id, snapshot_status],
        )
        .map_err(db_error)?;
    transaction.commit().map_err(db_error)?;
    let calculated = calculate_available_index(database, &snapshot_id)?;
    let report = WeeklyRunReport {
        generated_at: Utc::now().to_rfc3339(),
        basket_id: basket.basket_id,
        basket_sha256: current_hash,
        period_id,
        iso_year: week.iso_year,
        iso_week: week.iso_week,
        attempt_id,
        snapshot_id,
        snapshot_status: snapshot_status.to_owned(),
        price_context: "Generic".to_owned(),
        total_items: observations.len(),
        successful: success,
        errors,
        coverage_count_percent: percent(success, observations.len()),
        coverage_base_value_percent: percent(success, observations.len()),
        full_index: calculated.as_ref().and_then(|value| value.index.clone()),
        comparable_subset_index: calculated
            .as_ref()
            .and_then(|value| value.comparable_subset_index.clone()),
        warnings: if audit.status == "BasketVerified" {
            vec![]
        } else {
            vec!["полный индекс не опубликован: BlockedByBasketAudit".to_owned()]
        },
        observations,
    };
    save_weekly_exports(&report, database)?;
    log_event(
        &log_path,
        json!({"event":"finish", "attempt_id":report.attempt_id, "successful":success, "errors":errors, "duration_ms":started.elapsed().as_millis()}),
    )?;
    Ok(report)
}

fn calculate_available_index(
    database: &Database,
    snapshot_id: &str,
) -> Result<Option<index::IndexResult>, String> {
    let baseline: Option<String> = database
        .connection()
        .query_row(
            "SELECT id FROM baseline_periods WHERE status='BaselineReady' ORDER BY created_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?;
    let Some(baseline_id) = baseline else {
        return Ok(None);
    };
    let mut statement = database.connection().prepare(
        "SELECT p.source_product_id, bi.category_name, bi.base_quantity, bi.base_price_per_unit, wo.normalized_price
         FROM baseline_items bi JOIN products p ON p.id=bi.product_id
         LEFT JOIN snapshot_items si ON si.snapshot_id=?2 AND si.product_id=bi.product_id
         LEFT JOIN weekly_observations wo ON wo.id=si.observation_id
         WHERE bi.baseline_id=?1 ORDER BY p.source_product_id",
    ).map_err(db_error)?;
    let rows = statement
        .query_map(params![baseline_id, snapshot_id], |row| {
            Ok(IndexItem {
                product_id: row.get(0)?,
                category: row.get(1)?,
                quantity: row.get(2)?,
                base_unit_price: row.get(3)?,
                current_unit_price: row.get(4)?,
            })
        })
        .map_err(db_error)?;
    let items = rows.collect::<Result<Vec<_>, _>>().map_err(db_error)?;
    if items.is_empty() {
        Ok(None)
    } else {
        index::calculate(&items).map(Some)
    }
}

pub(crate) fn save_weekly_exports(
    report: &WeeklyRunReport,
    database: &Database,
) -> Result<(), String> {
    fs::create_dir_all("output").map_err(|error| error.to_string())?;
    fs::write(
        "output/weekly_prices.json",
        format!(
            "{}\n",
            serde_json::to_string_pretty(&report.observations).map_err(|error| error.to_string())?
        ),
    )
    .map_err(|error| error.to_string())?;
    let mut writer =
        csv::Writer::from_path("output/weekly_prices.csv").map_err(|error| error.to_string())?;
    for observation in &report.observations {
        writer
            .serialize(WeeklyCsvRow::from(observation))
            .map_err(|error| error.to_string())?;
    }
    writer.flush().map_err(|error| error.to_string())?;
    fs::write(
        "output/weekly_report.json",
        format!(
            "{}\n",
            serde_json::to_string_pretty(report).map_err(|error| error.to_string())?
        ),
    )
    .map_err(|error| error.to_string())?;
    let index_text = report
        .full_index
        .as_deref()
        .map(|value| format!("{value} (базовая неделя)"))
        .unwrap_or_else(|| "недоступен".to_owned());
    let markdown = format!(
        "# Недельный отчёт\n\nПериод: {}-W{:02} (Europe/Moscow). Корзина: `{}`.\n\nСтатус: `{}`. Получено: {}/{}; ошибок: {}; покрытие: {}%.\n\nПолный индекс: **{}**. {}\n",
        report.iso_year,
        report.iso_week,
        report.basket_id,
        report.snapshot_status,
        report.successful,
        report.total_items,
        report.errors,
        report.coverage_count_percent,
        index_text,
        report.warnings.join(" ")
    );
    fs::write("output/weekly_report.md", markdown).map_err(|error| error.to_string())?;
    let mut promo =
        csv::Writer::from_path("output/promotion_events.csv").map_err(|error| error.to_string())?;
    for observation in report
        .observations
        .iter()
        .filter(|item| item.old_price.is_some() || item.discount_info.is_some())
    {
        promo
            .serialize(WeeklyCsvRow::from(observation))
            .map_err(|error| error.to_string())?;
    }
    promo.flush().map_err(|error| error.to_string())?;

    let mut basket_history =
        csv::Writer::from_path("output/basket_history.csv").map_err(|error| error.to_string())?;
    basket_history
        .write_record([
            "basket_id",
            "created_at",
            "approved_at",
            "status",
            "initial_total",
        ])
        .map_err(|error| error.to_string())?;
    let mut statement = database.connection().prepare("SELECT id, created_at, COALESCE(approved_at,''), status, initial_total FROM baskets ORDER BY created_at").map_err(db_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(db_error)?;
    for row in rows {
        basket_history
            .serialize(row.map_err(db_error)?)
            .map_err(|error| error.to_string())?;
    }
    basket_history.flush().map_err(|error| error.to_string())?;

    let mut inflation =
        csv::Writer::from_path("output/inflation_index.csv").map_err(|error| error.to_string())?;
    inflation
        .write_record([
            "iso_year",
            "iso_week",
            "status",
            "full_index",
            "comparable_subset_index",
            "note",
        ])
        .map_err(|error| error.to_string())?;
    let publication_status = if report.full_index.is_some() {
        "Published"
    } else {
        "UnavailableWithoutCompleteBaseline"
    };
    inflation
        .serialize((
            report.iso_year,
            report.iso_week,
            publication_status,
            report.full_index.as_deref(),
            report.comparable_subset_index.as_deref(),
            if report.full_index.is_some() {
                "базовая неделя; недельное изменение ещё не рассчитывается"
            } else {
                "полный индекс не публикуется без полного baseline"
            },
        ))
        .map_err(|error| error.to_string())?;
    inflation.flush().map_err(|error| error.to_string())?;

    let mut products =
        csv::Writer::from_path("output/product_dynamics.csv").map_err(|error| error.to_string())?;
    products
        .write_record([
            "iso_year",
            "iso_week",
            "source_product_id",
            "group",
            "status",
            "current_normalized_price",
            "base_price",
            "change",
            "note",
        ])
        .map_err(|error| error.to_string())?;
    for item in &report.observations {
        let base_price: Option<String> = database.connection().query_row(
            "SELECT bi.base_price_per_unit FROM baseline_items bi JOIN baseline_periods bp ON bp.id=bi.baseline_id JOIN basket_versions bv ON bv.id=bp.basket_version_id JOIN products p ON p.id=bi.product_id WHERE bv.basket_id=?1 AND p.source_product_id=?2 ORDER BY bp.created_at DESC LIMIT 1",
            params![report.basket_id, item.source_product_id],
            |row| row.get(0),
        ).optional().map_err(db_error)?;
        let change = match (base_price.as_deref(), item.normalized_price.as_deref()) {
            (Some(base), Some(current)) => {
                let base = Decimal::from_str(base).map_err(|error| error.to_string())?;
                let current = Decimal::from_str(current).map_err(|error| error.to_string())?;
                Some(
                    ((current / base - Decimal::ONE) * Decimal::from(100))
                        .round_dp(6)
                        .normalize()
                        .to_string(),
                )
            }
            _ => None,
        };
        products
            .serialize((
                report.iso_year,
                report.iso_week,
                &item.source_product_id,
                &item.group,
                &item.status,
                item.normalized_price.as_deref(),
                base_price.as_deref(),
                change.as_deref(),
                if report.full_index.is_some() {
                    "baseline"
                } else {
                    "baseline недоступен"
                },
            ))
            .map_err(|error| error.to_string())?;
    }
    products.flush().map_err(|error| error.to_string())?;

    let mut categories = csv::Writer::from_path("output/category_dynamics.csv")
        .map_err(|error| error.to_string())?;
    categories
        .write_record([
            "iso_year",
            "iso_week",
            "category",
            "status",
            "base_cost",
            "current_cost",
            "index",
            "contribution",
        ])
        .map_err(|error| error.to_string())?;
    categories
        .serialize((
            report.iso_year,
            report.iso_week,
            "ALL",
            publication_status,
            Option::<&str>::None,
            Option::<&str>::None,
            report.full_index.as_deref(),
            report.full_index.as_deref().map(|_| "0"),
        ))
        .map_err(|error| error.to_string())?;
    categories.flush().map_err(|error| error.to_string())
}

pub fn load_weekly_report(path: &str) -> Result<WeeklyRunReport, String> {
    serde_json::from_str(&fs::read_to_string(path).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}

pub fn refresh_exports(database: &Database) -> Result<WeeklyRunReport, String> {
    let report = load_weekly_report("output/weekly_report.json")?;
    save_weekly_exports(&report, database)?;
    Ok(report)
}

pub fn phase2_stats(database: &Database) -> Result<Value, String> {
    let count = |table: &str| -> Result<i64, String> {
        database
            .connection()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .map_err(db_error)
    };
    Ok(json!({
        "basket_versions": count("basket_versions")?,
        "basket_audits": count("basket_audits")?,
        "baseline_periods": count("baseline_periods")?,
        "weekly_periods": count("weekly_periods")?,
        "collection_attempts": count("collection_attempts")?,
        "weekly_observations": count("weekly_observations")?,
        "weekly_snapshots": count("weekly_snapshots")?,
        "package_change_events": count("package_change_events")?,
        "price_condition_events": count("price_condition_events")?,
        "index_calculations": count("index_calculations")?
    }))
}

pub fn list_weeks(database: &Database, basket_id: Option<&str>) -> Result<Vec<Value>, String> {
    let mut statement = database.connection().prepare("SELECT wp.id,bv.basket_id,wp.iso_year,wp.iso_week,wp.timezone,wp.status,wp.starts_at_utc,wp.ends_at_utc FROM weekly_periods wp JOIN basket_versions bv ON bv.id=wp.basket_version_id WHERE (?1 IS NULL OR bv.basket_id=?1) ORDER BY wp.iso_year,wp.iso_week,wp.id").map_err(db_error)?;
    let rows = statement.query_map([basket_id], |row| Ok(json!({"id":row.get::<_,String>(0)?,"basket_id":row.get::<_,String>(1)?,"iso_year":row.get::<_,i32>(2)?,"iso_week":row.get::<_,u32>(3)?,"timezone":row.get::<_,String>(4)?,"status":row.get::<_,String>(5)?,"starts_at_utc":row.get::<_,String>(6)?,"ends_at_utc":row.get::<_,String>(7)?}))).map_err(db_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
}

fn classify_error(error: &str) -> &'static str {
    if error.contains("429") || error.contains("RateLimit") {
        "RateLimited"
    } else if error.contains("Transport") || error.contains("timeout") {
        "NetworkError"
    } else if error.contains("InvalidData") || error.contains("schema") {
        "InvalidResponse"
    } else if error.contains("not found") {
        "NotFound"
    } else {
        "Unavailable"
    }
}

fn percent(part: usize, total: usize) -> String {
    if total == 0 {
        return "0".to_owned();
    }
    (Decimal::from(100) * Decimal::from(part) / Decimal::from(total))
        .round_dp(4)
        .normalize()
        .to_string()
}

fn decimal_json(value: &Value) -> Option<String> {
    (!value.is_null())
        .then(|| Decimal::from_str(&value.to_string()).ok())
        .flatten()
        .map(|value| value.normalize().to_string())
}
fn db_error(error: rusqlite::Error) -> String {
    format!("ошибка SQLite: {error}")
}

fn log_event(path: &str, value: Value) -> Result<(), String> {
    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    writeln!(
        file,
        "{}",
        serde_json::to_string(&value).map_err(|error| error.to_string())?
    )
    .map_err(|error| error.to_string())
}

fn rotate_log(path: &str, maximum_bytes: u64) -> Result<(), String> {
    if fs::metadata(path).is_ok_and(|metadata| metadata.len() >= maximum_bytes) {
        let rotated = format!("{path}.1");
        if Path::new(&rotated).exists() {
            fs::remove_file(&rotated).map_err(|error| error.to_string())?;
        }
        fs::rename(path, rotated).map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn moscow_iso_week_handles_year_boundary() {
        let last = week_for(Utc.with_ymd_and_hms(2027, 1, 3, 12, 0, 0).unwrap());
        let next = week_for(Utc.with_ymd_and_hms(2027, 1, 4, 12, 0, 0).unwrap());
        assert_eq!((last.iso_year, last.iso_week), (2026, 53));
        assert_eq!((next.iso_year, next.iso_week), (2027, 1));
        assert!(last.starts_at_utc.ends_with("Z") || last.starts_at_utc.contains("+00:00"));
    }

    #[test]
    fn statuses_distinguish_network_rate_limit_and_schema() {
        assert_eq!(classify_error("HTTP 429"), "RateLimited");
        assert_eq!(classify_error("Transport timeout"), "NetworkError");
        assert_eq!(classify_error("InvalidData schema"), "InvalidResponse");
    }

    #[test]
    fn lock_prevents_concurrent_collection_and_recovers_on_drop() {
        let path = format!("target/weekly-test-{}.lock", std::process::id());
        let first = CollectionLock::acquire(&path).unwrap();
        assert!(CollectionLock::acquire(&path).is_err());
        drop(first);
        assert!(CollectionLock::acquire(&path).is_ok());
        fs::remove_file(path).unwrap();
    }
}
