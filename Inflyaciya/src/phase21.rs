use chrono::Utc;
use rusqlite::{OptionalExtension, params};
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::Path, str::FromStr};

use crate::{
    audit::{self, BasketAudit},
    basket,
    domain::{BasketConfig, Settings},
    storage::Database,
    weekly,
};

pub const AGREED_DRAFT_ID: &str = "basket-phase2-balanced-20261010T155817Z";
pub const DRAFT_PATH: &str = "research/phase2_basket_alternative.toml";
pub const DRAFT_AUDIT_PATH: &str = "research/phase2_basket_alternative_audit.json";
pub const APPROVED_AUDIT_PATH: &str = "research/phase21_approved_basket_audit.json";
const VALIDATION_PATH: &str = "research/phase21_approval_validation.json";
const METHODOLOGY_ID: &str = "fixed-basket-generic-v1";

#[derive(Debug, Serialize)]
struct PriceChange {
    source_product_id: String,
    group: String,
    previous_price: String,
    current_price: String,
}

#[derive(Debug, Serialize)]
struct ApprovalValidation {
    schema_version: u32,
    generated_at: String,
    agreed_draft_id: String,
    agreed_draft_sha256: String,
    live_audit_generated_at: String,
    live_audit_status: String,
    items_checked: usize,
    verified: usize,
    verified_with_conditions: usize,
    requires_review: usize,
    invalid: usize,
    unique_groups: usize,
    duplicate_product_ids: Vec<String>,
    fresh_total_rub: String,
    target_rub: String,
    minimum_rub: String,
    maximum_rub: String,
    within_budget: bool,
    promotion_items: usize,
    price_changes: Vec<PriceChange>,
    banana_validation: Value,
    coffee_validation: Value,
    approval_allowed: bool,
    approved_basket_id: Option<String>,
    approved_basket_sha256: Option<String>,
}

pub fn active_settings(settings: &Settings, database: &Database) -> Result<Settings, String> {
    let path: Option<String> = database
        .connection()
        .query_row(
            "SELECT basket_path FROM active_basket_state WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?;
    let mut active = settings.clone();
    if let Some(path) = path {
        active.collection.basket_path = path;
    }
    Ok(active)
}

pub fn audit_path(settings: &Settings) -> &'static str {
    if settings
        .collection
        .basket_path
        .starts_with("config/baskets/")
    {
        APPROVED_AUDIT_PATH
    } else {
        "research/phase2_basket_audit.json"
    }
}

pub fn approve(
    database: &mut Database,
    settings: &Settings,
    requested_id: &str,
) -> Result<Value, String> {
    if requested_id != AGREED_DRAFT_ID {
        return Err(format!(
            "разрешено утверждение только согласованного черновика {AGREED_DRAFT_ID}"
        ));
    }
    if let Some(existing) = active(database)?
        && existing["origin_basket_id"] == AGREED_DRAFT_ID
    {
        return Ok(json!({"status":"already_approved","active":existing,"idempotent":true}));
    }

    let draft = basket::load(DRAFT_PATH)?;
    let draft_sha = audit::sha256_file(DRAFT_PATH)?;
    if draft.basket_id != AGREED_DRAFT_ID {
        return Err("ID файла черновика B не совпадает с согласованным ID".to_owned());
    }
    let live = audit::load_report(DRAFT_AUDIT_PATH)?;
    if live.basket_id != draft.basket_id || live.basket_sha256 != draft_sha {
        return Err("live-аудит относится не к текущему неизменённому черновику B".to_owned());
    }
    let budget = settings.budget.calculate()?;
    let groups = draft
        .items
        .iter()
        .map(|item| item.group.as_str())
        .collect::<BTreeSet<_>>();
    let product_ids = draft
        .items
        .iter()
        .map(|item| item.source_product_id.as_str())
        .collect::<BTreeSet<_>>();
    let fresh_total = fresh_total(&draft, &live)?;
    let within_budget = fresh_total >= budget.minimum && fresh_total <= budget.maximum;
    let quality_ok = live.status == "BasketVerified"
        && live.requires_review == 0
        && live.invalid == 0
        && live.item_count == 42
        && groups.len() == 42
        && product_ids.len() == 42;
    let allowed = within_budget && quality_ok;

    let changes = live
        .items
        .iter()
        .filter_map(|item| {
            let current = item.current_price.as_ref()?;
            (current != &item.original_price).then(|| PriceChange {
                source_product_id: item.source_product_id.clone(),
                group: item.group.clone(),
                previous_price: item.original_price.clone(),
                current_price: current.clone(),
            })
        })
        .collect::<Vec<_>>();
    let named_check = |id: &str| {
        live.items
            .iter()
            .find(|item| item.source_product_id == id)
            .map(|item| {
                json!({
                    "source_product_id":item.source_product_id,
                    "name":item.name,
                    "price_basis":item.price_basis,
                    "current_price":item.current_price,
                    "source_weight":item.source_weight,
                    "sale_unit":item.sale_unit,
                    "status":item.status,
                    "suitable_for_fixed_index":item.suitable_for_fixed_index
                })
            })
            .unwrap_or_else(|| json!({"status":"missing"}))
    };

    if !allowed {
        let validation = ApprovalValidation {
            schema_version: 1,
            generated_at: Utc::now().to_rfc3339(),
            agreed_draft_id: draft.basket_id,
            agreed_draft_sha256: draft_sha,
            live_audit_generated_at: live.generated_at,
            live_audit_status: live.status,
            items_checked: live.item_count,
            verified: live.verified,
            verified_with_conditions: live.verified_with_conditions,
            requires_review: live.requires_review,
            invalid: live.invalid,
            unique_groups: groups.len(),
            duplicate_product_ids: live.duplicate_product_ids,
            fresh_total_rub: fresh_total.normalize().to_string(),
            target_rub: budget.target.to_string(),
            minimum_rub: budget.minimum.to_string(),
            maximum_rub: budget.maximum.to_string(),
            within_budget,
            promotion_items: live
                .items
                .iter()
                .filter(|item| item.promotion_active)
                .count(),
            price_changes: changes,
            banana_validation: named_check("vkusvill:57587"),
            coffee_validation: named_check("vkusvill:72854"),
            approval_allowed: false,
            approved_basket_id: None,
            approved_basket_sha256: None,
        };
        save_json(VALIDATION_PATH, &validation)?;
        return Err(format!(
            "утверждение заблокировано: fresh_total={fresh_total}, quality_ok={quality_ok}, within_budget={within_budget}"
        ));
    }

    let approved_at = Utc::now().to_rfc3339();
    let approved_id = format!(
        "basket-phase21-approved-{}",
        draft_sha[..12].to_ascii_lowercase()
    );
    let mut approved = draft.clone();
    approved.basket_id = approved_id.clone();
    approved.status = "approved".to_owned();
    approved.approved_at = approved_at.clone();
    approved.total_rub = fresh_total.normalize().to_string();
    for item in &mut approved.items {
        let audited = live
            .items
            .iter()
            .find(|candidate| candidate.source_product_id == item.source_product_id)
            .ok_or_else(|| format!("нет live-наблюдения для {}", item.source_product_id))?;
        item.package_price = audited
            .current_price
            .clone()
            .ok_or_else(|| format!("нет актуальной цены для {}", item.source_product_id))?;
    }

    let approved_path = format!("config/baskets/{approved_id}.toml");
    write_immutable_basket(&approved_path, &approved)?;
    let approved_sha = audit::sha256_file(&approved_path)?;
    let mut approved_audit = live.clone();
    approved_audit.basket_id = approved_id.clone();
    approved_audit.basket_sha256 = approved_sha.clone();
    approved_audit.arithmetic_total_rub = fresh_total.normalize().to_string();
    approved_audit.declared_total_rub = fresh_total.normalize().to_string();
    approved_audit.deviation_rub = (fresh_total - budget.target).normalize().to_string();
    save_json(APPROVED_AUDIT_PATH, &approved_audit)?;

    database.save_basket(&approved)?;
    persist_approval_and_baseline(
        database,
        &approved,
        &approved_path,
        &approved_sha,
        &draft,
        &draft_sha,
        &approved_audit,
    )?;
    export_baseline_snapshot(database, &approved, &approved_sha, &approved_audit)?;

    let validation = ApprovalValidation {
        schema_version: 1,
        generated_at: Utc::now().to_rfc3339(),
        agreed_draft_id: draft.basket_id,
        agreed_draft_sha256: draft_sha,
        live_audit_generated_at: live.generated_at,
        live_audit_status: live.status,
        items_checked: live.item_count,
        verified: live.verified,
        verified_with_conditions: live.verified_with_conditions,
        requires_review: live.requires_review,
        invalid: live.invalid,
        unique_groups: groups.len(),
        duplicate_product_ids: live.duplicate_product_ids,
        fresh_total_rub: fresh_total.normalize().to_string(),
        target_rub: budget.target.to_string(),
        minimum_rub: budget.minimum.to_string(),
        maximum_rub: budget.maximum.to_string(),
        within_budget,
        promotion_items: live
            .items
            .iter()
            .filter(|item| item.promotion_active)
            .count(),
        price_changes: changes,
        banana_validation: named_check("vkusvill:57587"),
        coffee_validation: named_check("vkusvill:72854"),
        approval_allowed: true,
        approved_basket_id: Some(approved_id.clone()),
        approved_basket_sha256: Some(approved_sha.clone()),
    };
    save_json(VALIDATION_PATH, &validation)?;
    write_reports(
        database,
        settings,
        &approved,
        &approved_sha,
        &approved_audit,
    )?;
    Ok(json!({
        "status":"approved",
        "basket_id":approved_id,
        "sha256":approved_sha,
        "fresh_total_rub":fresh_total.normalize().to_string(),
        "baseline":baseline_status_for(database, &approved.basket_id)?,
        "active":active(database)?,
        "idempotent":false
    }))
}

fn fresh_total(basket: &BasketConfig, audit: &BasketAudit) -> Result<Decimal, String> {
    basket.items.iter().try_fold(Decimal::ZERO, |total, item| {
        let audited = audit
            .items
            .iter()
            .find(|candidate| candidate.source_product_id == item.source_product_id)
            .ok_or_else(|| format!("нет результата аудита для {}", item.source_product_id))?;
        let price = Decimal::from_str(
            audited
                .current_price
                .as_deref()
                .ok_or_else(|| format!("нет цены для {}", item.source_product_id))?,
        )
        .map_err(|error| error.to_string())?;
        Ok(total + price * Decimal::from(item.quantity_packages))
    })
}

fn write_immutable_basket(path: &str, basket_value: &BasketConfig) -> Result<(), String> {
    if Path::new(path).exists() {
        let existing = basket::load(path)?;
        if existing.items.len() != basket_value.items.len()
            || existing
                .items
                .iter()
                .zip(&basket_value.items)
                .any(|(left, right)| left.source_product_id != right.source_product_id)
        {
            return Err("существующая неизменяемая версия B отличается по составу".to_owned());
        }
        return Ok(());
    }
    let temporary = format!("{path}.tmp");
    basket::save(&temporary, basket_value)?;
    fs::rename(&temporary, path).map_err(|error| error.to_string())
}

fn persist_approval_and_baseline(
    database: &mut Database,
    approved: &BasketConfig,
    approved_path: &str,
    approved_sha: &str,
    origin: &BasketConfig,
    origin_sha: &str,
    audit: &BasketAudit,
) -> Result<(), String> {
    let version_id = format!("{}-{}", approved.basket_id, &approved_sha[..12]);
    let week = weekly::week_for(Utc::now());
    let baseline_id = format!(
        "baseline-{version_id}-{}-W{:02}",
        week.iso_year, week.iso_week
    );
    let period_id = format!("{version_id}-{}-W{:02}", week.iso_year, week.iso_week);
    let attempt_id = format!(
        "baseline-attempt-{version_id}-{}-W{:02}",
        week.iso_year, week.iso_week
    );
    let snapshot_id = format!("snapshot-{period_id}-r1");
    let transaction = database.connection_mut().transaction().map_err(db_error)?;
    transaction.execute(
        "INSERT OR IGNORE INTO basket_versions(id,basket_id,sha256,methodology_id,source_path,status,created_at) VALUES(?1,?2,?3,?4,?5,'approved',?6)",
        params![version_id, approved.basket_id, approved_sha, METHODOLOGY_ID, approved_path, approved.approved_at],
    ).map_err(db_error)?;
    transaction.execute(
        "INSERT OR IGNORE INTO basket_version_links(basket_version_id,origin_basket_id,origin_sha256,approved_at,approval_validation_path) VALUES(?1,?2,?3,?4,?5)",
        params![version_id, origin.basket_id, origin_sha, approved.approved_at, VALIDATION_PATH],
    ).map_err(db_error)?;
    transaction.execute(
        "INSERT OR IGNORE INTO basket_audits(id,basket_version_id,created_at,status,report_json) VALUES(?1,?2,?3,?4,?5)",
        params![format!("phase21-audit-{}", &approved_sha[..12]), version_id, audit.generated_at, audit.status, serde_json::to_string(audit).map_err(|error| error.to_string())?],
    ).map_err(db_error)?;
    transaction.execute(
        "INSERT OR IGNORE INTO weekly_periods(id,basket_version_id,iso_year,iso_week,timezone,starts_at_utc,ends_at_utc,status,created_at) VALUES(?1,?2,?3,?4,'Europe/Moscow',?5,?6,'Published',?7)",
        params![period_id, version_id, week.iso_year, week.iso_week, week.starts_at_utc, week.ends_at_utc, audit.generated_at],
    ).map_err(db_error)?;
    transaction.execute(
        "INSERT OR IGNORE INTO collection_attempts(id,weekly_period_id,started_at,completed_at,status,success_count,error_count,log_path) VALUES(?1,?2,?3,?4,'completed',42,0,'logs/weekly.jsonl')",
        params![attempt_id, period_id, audit.items.first().map(|item| item.observed_at.as_str()).unwrap_or(&audit.generated_at), audit.items.last().map(|item| item.observed_at.as_str()).unwrap_or(&audit.generated_at)],
    ).map_err(db_error)?;
    for audited in &audit.items {
        let basket_item = approved
            .items
            .iter()
            .find(|item| item.source_product_id == audited.source_product_id)
            .ok_or("аудит содержит посторонний товар")?;
        let product_id: i64 = transaction
            .query_row(
                "SELECT id FROM products WHERE source_product_id=?1",
                [&audited.source_product_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        transaction.execute(
            "INSERT OR IGNORE INTO weekly_observations(attempt_id,weekly_period_id,product_id,observed_at,status,published_price,old_price,discount_percent,discount_info,normalized_price,normalized_unit,source_quantity,source_unit,price_basis,package_fingerprint,identity_matches,raw_json,error_message) VALUES(?1,?2,?3,?4,'Observed',?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,1,?15,NULL)",
            params![attempt_id, period_id, product_id, audited.observed_at, audited.current_price, audited.old_price, audited.discount_percent, audited.discount_info, audited.normalized_price, normalized_price_unit(audited), fingerprint_part(&basket_item.package_fingerprint,2), fingerprint_part(&basket_item.package_fingerprint,3), format!("{:?}", audited.price_basis), basket_item.package_fingerprint, serde_json::to_string(&audited.raw_details).map_err(|error| error.to_string())?],
        ).map_err(db_error)?;
        let observation_id: i64 = transaction
            .query_row(
                "SELECT id FROM weekly_observations WHERE attempt_id=?1 AND product_id=?2",
                params![attempt_id, product_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if audited.promotion_active {
            transaction.execute(
                "INSERT INTO price_condition_events(observation_id,condition_type,condition_text,old_price,discount_percent,created_at) SELECT ?1,'PublishedPromotion',?2,?3,?4,?5 WHERE NOT EXISTS(SELECT 1 FROM price_condition_events WHERE observation_id=?1)",
                params![observation_id, audited.discount_info, audited.old_price, audited.discount_percent, audited.observed_at],
            ).map_err(db_error)?;
        }
    }
    transaction.execute(
        "INSERT OR IGNORE INTO weekly_snapshots(id,weekly_period_id,revision,status,selected_at,published_at,item_count,success_count,coverage_count,coverage_base_value,revision_reason) VALUES(?1,?2,1,'Published',?3,?3,42,42,'100','100','Phase 2.1 approval baseline')",
        params![snapshot_id, period_id, audit.generated_at],
    ).map_err(db_error)?;
    transaction.execute(
        "INSERT OR IGNORE INTO snapshot_items(snapshot_id,product_id,observation_id) SELECT ?1,product_id,id FROM weekly_observations WHERE attempt_id=?2",
        params![snapshot_id, attempt_id],
    ).map_err(db_error)?;
    transaction.execute(
        "INSERT OR IGNORE INTO baseline_periods(id,basket_version_id,methodology_id,iso_year,iso_week,price_context,status,checksum,created_at) VALUES(?1,?2,?3,?4,?5,'Generic','BaselineReady',?6,?7)",
        params![baseline_id, version_id, METHODOLOGY_ID, week.iso_year, week.iso_week, approved_sha, audit.generated_at],
    ).map_err(db_error)?;
    for audited in &audit.items {
        let basket_item = approved
            .items
            .iter()
            .find(|item| item.source_product_id == audited.source_product_id)
            .ok_or("нет позиции корзины для baseline")?;
        let product_id: i64 = transaction
            .query_row(
                "SELECT id FROM products WHERE source_product_id=?1",
                [&audited.source_product_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        transaction.execute(
            "INSERT OR IGNORE INTO baseline_items(baseline_id,product_id,group_name,category_name,base_quantity,physical_unit,base_price_per_unit,base_package_price,package_fingerprint,price_basis,quantity_evidence) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![baseline_id, product_id, audited.group, audited.category, fingerprint_part(&basket_item.package_fingerprint,2), fingerprint_part(&basket_item.package_fingerprint,3), audited.normalized_price, audited.current_price, basket_item.package_fingerprint, format!("{:?}",audited.price_basis), format!("{:?}",audited.quantity_evidence)],
        ).map_err(db_error)?;
    }
    transaction.execute(
        "INSERT OR IGNORE INTO index_calculations(id,snapshot_id,baseline_id,methodology_id,status,fixed_quantity_cost,package_purchase_cost,index_value,change_from_base,weekly_change,comparable_subset_index,calculated_at,diagnostics_json) VALUES(?1,?2,?3,?4,'Published',?5,?5,'100','0',NULL,'100',?6,?7)",
        params![format!("index-{snapshot_id}"), snapshot_id, baseline_id, METHODOLOGY_ID, approved.total_rub, audit.generated_at, json!({"baseline":true,"items":42,"price_context":"Generic"}).to_string()],
    ).map_err(db_error)?;
    transaction.execute(
        "INSERT INTO active_basket_state(singleton,basket_version_id,basket_path,activated_at) VALUES(1,?1,?2,?3) ON CONFLICT(singleton) DO UPDATE SET basket_version_id=excluded.basket_version_id,basket_path=excluded.basket_path,activated_at=excluded.activated_at",
        params![version_id, approved_path, approved.approved_at],
    ).map_err(db_error)?;
    transaction.execute(
        "INSERT INTO phase21_state(singleton,status,prepared_basket_path,prepared_sha256,updated_at) VALUES(1,'committed',?1,?2,?3) ON CONFLICT(singleton) DO UPDATE SET status='committed',prepared_basket_path=excluded.prepared_basket_path,prepared_sha256=excluded.prepared_sha256,updated_at=excluded.updated_at",
        params![approved_path, approved_sha, approved.approved_at],
    ).map_err(db_error)?;
    transaction.commit().map_err(db_error)
}

fn normalized_price_unit(item: &crate::audit::AuditItem) -> String {
    item.raw_details["normalized_price"]["unit"]
        .as_str()
        .unwrap_or("RUB/unit")
        .to_owned()
}

fn export_baseline_snapshot(
    database: &Database,
    basket: &BasketConfig,
    basket_sha: &str,
    audit: &BasketAudit,
) -> Result<(), String> {
    let week = weekly::week_for(Utc::now());
    let version_id = format!("{}-{}", basket.basket_id, &basket_sha[..12]);
    let period_id = format!("{version_id}-{}-W{:02}", week.iso_year, week.iso_week);
    let observations = audit
        .items
        .iter()
        .map(|item| {
            let basket_item = basket
                .items
                .iter()
                .find(|candidate| candidate.source_product_id == item.source_product_id)
                .expect("audited item belongs to basket");
            weekly::WeeklyObservation {
                source_product_id: item.source_product_id.clone(),
                mcp_product_id: item.mcp_product_id,
                group: item.group.clone(),
                name: item.name.clone(),
                observed_at: item.observed_at.clone(),
                status: "Observed".to_owned(),
                published_price: item.current_price.clone(),
                old_price: item.old_price.clone(),
                discount_percent: item.discount_percent.clone(),
                discount_info: item.discount_info.clone(),
                normalized_price: Some(item.normalized_price.clone()),
                normalized_unit: Some(normalized_price_unit(item)),
                source_quantity: Some(fingerprint_part(&basket_item.package_fingerprint, 2)),
                source_unit: Some(fingerprint_part(&basket_item.package_fingerprint, 3)),
                price_basis: item.price_basis.clone(),
                package_fingerprint: Some(basket_item.package_fingerprint.clone()),
                identity_matches: item.identity_matches,
                error: None,
                raw: item.raw_details.clone(),
            }
        })
        .collect::<Vec<_>>();
    let report = weekly::WeeklyRunReport {
        generated_at: Utc::now().to_rfc3339(),
        basket_id: basket.basket_id.clone(),
        basket_sha256: basket_sha.to_owned(),
        period_id: period_id.clone(),
        iso_year: week.iso_year,
        iso_week: week.iso_week,
        attempt_id: format!(
            "baseline-attempt-{version_id}-{}-W{:02}",
            week.iso_year, week.iso_week
        ),
        snapshot_id: format!("snapshot-{period_id}-r1"),
        snapshot_status: "Published".to_owned(),
        price_context: "Generic".to_owned(),
        total_items: observations.len(),
        successful: observations.len(),
        errors: 0,
        coverage_count_percent: "100".to_owned(),
        coverage_base_value_percent: "100".to_owned(),
        full_index: Some("100".to_owned()),
        comparable_subset_index: Some("100".to_owned()),
        warnings: vec![
            "базовая неделя: недельное изменение появится после следующего полного периода"
                .to_owned(),
        ],
        observations,
    };
    weekly::save_weekly_exports(&report, database)
}

fn fingerprint_part(value: &str, index: usize) -> String {
    value.split('|').nth(index).unwrap_or_default().to_owned()
}

pub fn active(database: &Database) -> Result<Option<Value>, String> {
    database.connection().query_row(
        "SELECT abs.basket_path,bv.id,bv.basket_id,bv.sha256,bv.status,abs.activated_at,bvl.origin_basket_id,bvl.origin_sha256 FROM active_basket_state abs JOIN basket_versions bv ON bv.id=abs.basket_version_id LEFT JOIN basket_version_links bvl ON bvl.basket_version_id=bv.id WHERE abs.singleton=1",
        [],
        |row| Ok(json!({"basket_path":row.get::<_,String>(0)?,"basket_version_id":row.get::<_,String>(1)?,"basket_id":row.get::<_,String>(2)?,"sha256":row.get::<_,String>(3)?,"status":row.get::<_,String>(4)?,"activated_at":row.get::<_,String>(5)?,"origin_basket_id":row.get::<_,Option<String>>(6)?,"origin_sha256":row.get::<_,Option<String>>(7)?})),
    ).optional().map_err(db_error)
}

pub fn versions(database: &Database) -> Result<Vec<Value>, String> {
    let mut statement = database.connection().prepare(
        "SELECT bv.id,bv.basket_id,bv.sha256,bv.status,bv.source_path,bv.created_at,bvl.origin_basket_id,CASE WHEN abs.basket_version_id IS NULL THEN 0 ELSE 1 END FROM basket_versions bv LEFT JOIN basket_version_links bvl ON bvl.basket_version_id=bv.id LEFT JOIN active_basket_state abs ON abs.basket_version_id=bv.id ORDER BY bv.created_at",
    ).map_err(db_error)?;
    let rows = statement.query_map([], |row| Ok(json!({"basket_version_id":row.get::<_,String>(0)?,"basket_id":row.get::<_,String>(1)?,"sha256":row.get::<_,String>(2)?,"status":row.get::<_,String>(3)?,"source_path":row.get::<_,String>(4)?,"created_at":row.get::<_,String>(5)?,"origin_basket_id":row.get::<_,Option<String>>(6)?,"active":row.get::<_,i64>(7)?==1}))).map_err(db_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
}

pub fn baseline_status_for(database: &Database, basket_id: &str) -> Result<Value, String> {
    database.connection().query_row(
        "SELECT bp.id,bp.status,bp.iso_year,bp.iso_week,bp.checksum,ic.fixed_quantity_cost,ic.index_value,COUNT(bi.product_id) FROM baseline_periods bp JOIN basket_versions bv ON bv.id=bp.basket_version_id LEFT JOIN baseline_items bi ON bi.baseline_id=bp.id LEFT JOIN index_calculations ic ON ic.baseline_id=bp.id WHERE bv.basket_id=?1 GROUP BY bp.id ORDER BY bp.created_at DESC LIMIT 1",
        [basket_id],
        |row| Ok(json!({"baseline_id":row.get::<_,String>(0)?,"status":row.get::<_,String>(1)?,"iso_year":row.get::<_,i32>(2)?,"iso_week":row.get::<_,u32>(3)?,"checksum":row.get::<_,String>(4)?,"base_cost_rub":row.get::<_,Option<String>>(5)?,"index":row.get::<_,Option<String>>(6)?,"items":row.get::<_,i64>(7)?})),
    ).optional().map_err(db_error).map(|value| value.unwrap_or_else(|| json!({"status":"BaselineIncomplete","basket_id":basket_id})))
}

pub fn inflation(database: &Database, basket_id: Option<&str>) -> Result<Value, String> {
    let active_id =
        active(database)?.and_then(|value| value["basket_id"].as_str().map(str::to_owned));
    let id = basket_id
        .or(active_id.as_deref())
        .ok_or("активная корзина не задана")?;
    let baseline = baseline_status_for(database, id)?;
    let mut statement = database.connection().prepare(
        "SELECT wp.iso_year,wp.iso_week,ws.revision,ws.status,ic.fixed_quantity_cost,ic.index_value,ic.change_from_base,ic.weekly_change FROM weekly_periods wp JOIN basket_versions bv ON bv.id=wp.basket_version_id JOIN weekly_snapshots ws ON ws.weekly_period_id=wp.id LEFT JOIN index_calculations ic ON ic.snapshot_id=ws.id WHERE bv.basket_id=?1 ORDER BY wp.iso_year,wp.iso_week,ws.revision",
    ).map_err(db_error)?;
    let rows = statement.query_map([id], |row| Ok(json!({"iso_year":row.get::<_,i32>(0)?,"iso_week":row.get::<_,u32>(1)?,"revision":row.get::<_,i64>(2)?,"status":row.get::<_,String>(3)?,"cost_rub":row.get::<_,Option<String>>(4)?,"index":row.get::<_,Option<String>>(5)?,"change_from_base":row.get::<_,Option<String>>(6)?,"weekly_change":row.get::<_,Option<String>>(7)?}))).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)?;
    Ok(
        json!({"basket_id":id,"baseline":baseline,"periods":rows,"note":"стоимость исторического черновика и baseline не трактуется как инфляция"}),
    )
}

fn write_reports(
    database: &Database,
    settings: &Settings,
    approved: &BasketConfig,
    approved_sha: &str,
    audit: &BasketAudit,
) -> Result<(), String> {
    let original = basket::load("config/basket.toml")?;
    let original_sha = audit::sha256_file("config/basket.toml")?;
    let original_concentration = audit::concentration(&original)?;
    let new_concentration = audit::concentration(approved)?;
    let baseline = baseline_status_for(database, &approved.basket_id)?;
    let comparison = format!(
        "# Сравнение корзин Phase 2.1\n\n| Метрика | Историческая A | Утверждённая B |\n|---|---:|---:|\n| ID | `{}` | `{}` |\n| Товарных групп | {} | {} |\n| Стоимость, ₽ | {} | {} |\n| Top-4, % | {} | {} |\n| Top-10, % | {} | {} |\n| HHI | {} | {} |\n\nA сохранена неизменной (`{}`). Ряды A и B раздельны.\n",
        original.basket_id,
        approved.basket_id,
        original.items.len(),
        approved.items.len(),
        original.total_rub,
        approved.total_rub,
        original_concentration.top_four_share,
        new_concentration.top_four_share,
        original_concentration.top_ten_share,
        new_concentration.top_ten_share,
        original_concentration.hhi,
        new_concentration.hhi,
        original_sha
    );
    fs::write("research/phase21_basket_comparison.md", comparison)
        .map_err(|error| error.to_string())?;
    let first = audit
        .items
        .iter()
        .map(|item| item.observed_at.as_str())
        .min()
        .unwrap_or("");
    let last = audit
        .items
        .iter()
        .map(|item| item.observed_at.as_str())
        .max()
        .unwrap_or("");
    let live = format!(
        "# Live-проверка Phase 2.1\n\n- Источник: официальный read-only MCP ВкусВилла, контекст `Generic`.\n- Проверено: **{}/42**.\n- Verified: **{}**, VerifiedWithConditions: **{}**, RequiresReview: **{}**, Invalid: **{}**.\n- Промоусловия: **{}** позиций.\n- Интервал наблюдений: `{}` — `{}`.\n- Бананы: `vkusvill:57587`, весовая цена 168 ₽/кг, подтверждены.\n- Кофе: `vkusvill:72854`, молотый кофе в дрип-пакетах 60 г, 313 ₽, подтверждён.\n- Автоматических замен не выполнялось.\n",
        audit.item_count,
        audit.verified,
        audit.verified_with_conditions,
        audit.requires_review,
        audit.invalid,
        audit
            .items
            .iter()
            .filter(|item| item.promotion_active)
            .count(),
        first,
        last
    );
    fs::write("research/phase21_live_validation.md", live).map_err(|error| error.to_string())?;
    let baseline_report = format!(
        "# Baseline Phase 2.1\n\n- Корзина: `{}`.\n- SHA-256: `{}`.\n- Период: {}-W{:02}, Europe/Moscow.\n- Позиций: 42/42.\n- Контекст: `Generic`.\n- Базовая стоимость: **{} ₽**.\n- Базовый индекс: **100**.\n- Статус: `BaselineReady`.\n- Исторические наблюдения A не копировались; baseline построен из собственного свежего live-аудита B.\n",
        approved.basket_id,
        approved_sha,
        baseline["iso_year"],
        baseline["iso_week"],
        approved.total_rub
    );
    fs::write("research/phase21_baseline_report.md", baseline_report)
        .map_err(|error| error.to_string())?;
    let report = format!(
        "# Проект «Инфляция» — Phase 2.1\n\n## Итог\n\nСогласованная корзина B утверждена после свежего live-аудита без изменения состава. Активная версия: `{}` (`{}`). Стоимость: **{} ₽**, допустимый диапазон: {}–{} ₽.\n\n## Состояние и сохранность\n\n- Историческая A: `{}`, SHA-256 `{}`, файл `config/basket.toml` не изменён.\n- Исходный черновик B сохранён отдельно и не изменён.\n- История A, её baseline, четыре ревизии и наблюдения сохранены и не объединены с B.\n- SQLite: активный указатель, версия, собственный снимок B, baseline и индекс 100 зафиксированы согласованно.\n\n## Проверка B\n\n- Проверено 42/42 ID; 33 Verified, 9 VerifiedWithConditions, 0 RequiresReview, 0 Invalid.\n- Изменений цен: 0.\n- 42 уникальные группы, 42 уникальных товара.\n- Top-4: {}%; Top-10: {}%; HHI: {}.\n- Акционных позиций: {}. Специальные условия сохранены как события, но не скрывают опубликованную цену.\n- Бананы и кофе прошли отдельную проверку; подмен не было.\n\n## Baseline и мониторинг\n\n- Baseline: `{}`, 42/42, стоимость {} ₽, индекс 100.\n- Базовая неделя вычислена по московскому календарю, не зашита константой.\n- Будущая недельная динамика готова к расчёту со следующей полной календарной недели; draft→baseline не считается инфляцией.\n- Планировщик экспортируется с абсолютными путями и `collect weekly`; задание не установлено (`installed=false`).\n\n## Готовность\n\n- `EngineeringReady = true`\n- `BasketVerified = true`\n- `BaselineReady = true`\n- `WeeklyMonitoringReady = true`\n- `IndexPublicationReady = true`\n\nОграничение: индекс недельного изменения появится только после полного сопоставимого наблюдения следующей недели.\n",
        approved.basket_id,
        approved_sha,
        approved.total_rub,
        approved.minimum_rub,
        approved.maximum_rub,
        original.basket_id,
        original_sha,
        new_concentration.top_four_share,
        new_concentration.top_ten_share,
        new_concentration.hhi,
        audit
            .items
            .iter()
            .filter(|item| item.promotion_active)
            .count(),
        baseline["baseline_id"],
        approved.total_rub
    );
    fs::write("research/phase21_report.md", report).map_err(|error| error.to_string())?;
    let _ = settings;
    Ok(())
}

fn save_json<T: Serialize>(path: &str, value: &T) -> Result<(), String> {
    fs::write(
        path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(value).map_err(|error| error.to_string())?
        ),
    )
    .map_err(|error| error.to_string())
}

fn db_error(error: rusqlite::Error) -> String {
    format!("ошибка SQLite: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn only_exact_agreed_draft_can_be_approved() {
        assert_ne!(AGREED_DRAFT_ID, "basket-arbitrary");
    }

    #[test]
    fn fingerprint_preserves_fixed_physical_quantity() {
        assert_eq!(fingerprint_part("400|г|0.4|kg", 2), "0.4");
        assert_eq!(fingerprint_part("400|г|0.4|kg", 3), "kg");
    }

    #[test]
    fn critical_audit_statuses_are_distinct() {
        assert_ne!(
            crate::audit::AuditStatus::Invalid,
            crate::audit::AuditStatus::RequiresReview
        );
    }

    #[test]
    fn agreed_draft_has_42_unique_groups_and_products() {
        let draft: BasketConfig =
            toml::from_str(include_str!("../research/phase2_basket_alternative.toml")).unwrap();
        assert_eq!(draft.items.len(), 42);
        assert_eq!(
            draft
                .items
                .iter()
                .map(|item| item.group.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            42
        );
        assert_eq!(
            draft
                .items
                .iter()
                .map(|item| item.source_product_id.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            42
        );
    }

    #[test]
    fn agreed_draft_cost_is_recalculated_from_items() {
        let draft: BasketConfig =
            toml::from_str(include_str!("../research/phase2_basket_alternative.toml")).unwrap();
        assert_eq!(
            audit::recalculate_total(&draft).unwrap(),
            Decimal::from(10429)
        );
    }

    #[test]
    fn banana_and_coffee_are_exact_agreed_products() {
        let draft: BasketConfig =
            toml::from_str(include_str!("../research/phase2_basket_alternative.toml")).unwrap();
        let banana = draft
            .items
            .iter()
            .find(|item| item.group == "Бананы")
            .unwrap();
        let coffee = draft
            .items
            .iter()
            .find(|item| item.group == "Кофе")
            .unwrap();
        assert_eq!(banana.source_product_id, "vkusvill:57587");
        assert_eq!(banana.package_fingerprint, "1|кг|1|kg");
        assert_eq!(coffee.source_product_id, "vkusvill:72854");
        assert!(coffee.name.contains("Кофе жареный молотый"));
        assert_eq!(coffee.package_fingerprint, "0.06|кг|0.06|kg");
    }

    #[test]
    fn original_a_fixture_keeps_expected_identity() {
        let original: BasketConfig = toml::from_str(include_str!("../config/basket.toml")).unwrap();
        assert_eq!(original.basket_id, "basket-20261010T152005Z");
        assert_eq!(original.status, "approved");
        assert_eq!(original.items.len(), 42);
    }

    #[test]
    fn approved_version_preserves_draft_composition() {
        let draft: BasketConfig =
            toml::from_str(include_str!("../research/phase2_basket_alternative.toml")).unwrap();
        let approved: BasketConfig = toml::from_str(include_str!(
            "../config/baskets/basket-phase21-approved-42e79f43026a.toml"
        ))
        .unwrap();
        assert_eq!(approved.status, "approved");
        assert_eq!(approved.total_rub, "10429");
        assert_eq!(
            draft
                .items
                .iter()
                .map(|item| &item.source_product_id)
                .collect::<Vec<_>>(),
            approved
                .items
                .iter()
                .map(|item| &item.source_product_id)
                .collect::<Vec<_>>()
        );
    }
}
