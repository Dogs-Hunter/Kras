use chrono::Utc;
use serde_json::json;
use std::fs;

use crate::{
    audit, basket,
    catalog::{load_catalog, load_settings},
    phase21, scheduler,
    storage::Database,
    weekly,
};

const SETTINGS_PATH: &str = "config/settings.toml";

pub fn is_phase2_command(value: &str) -> bool {
    matches!(
        value,
        "audit" | "basket" | "baseline" | "weeks" | "report" | "schedule" | "backup"
    )
}

pub fn print_help() {
    crate::phase1::print_help();
    println!(
        "\nКоманды Phase 2/2.1:\n  backup                 согласованная резервная копия SQLite\n  audit basket           live-аудит активной корзины\n  audit alternative      live-аудит согласованной B\n  basket approve --id ID утвердить только согласованную B\n  basket active          активная версия\n  basket versions        история версий\n  basket compare         сравнение A/B без создания нового черновика\n  baseline create        показать/создать baseline активной версии\n  baseline status        статус baseline активной версии\n  collect weekly         недельный сбор активной корзины\n  weeks [--id ID]        недельные периоды\n  report weekly          недельный отчёт\n  report inflation [--id ID] индекс выбранной версии\n  schedule export        экспорт задания Windows без установки"
    );
}

pub fn run(args: &[String]) -> Result<(), String> {
    let settings = load_settings(SETTINGS_PATH)?;
    match args.first().map(String::as_str) {
        Some("backup") if args.get(1).map(String::as_str) == Some("verify") => {
            let path = args.get(2).ok_or("укажите путь к резервной копии")?;
            let database = Database::open(path)?;
            print(&json!({
                "path": path,
                "integrity": database.integrity_check()?,
                "foreign_key_check": database.foreign_key_check()?,
                "migration_versions": database.migration_versions()?,
                "stats": database.stats()?,
                "phase2_stats": weekly::phase2_stats(&database)?,
                "sha256": audit::sha256_file(path)?
            }))
        }
        Some("backup") => backup(&settings.collection.database_path),
        Some("audit") if args.get(1).map(String::as_str) == Some("basket") => {
            let database = database(&settings.collection.database_path)?;
            let active_settings = phase21::active_settings(&settings, &database)?;
            let result = audit::run_live_for_path(
                &active_settings,
                &active_settings.collection.basket_path,
            )?;
            println!(
                "{}",
                serde_json::to_string_pretty(&result).map_err(|error| error.to_string())?
            );
            Ok(())
        }
        Some("audit") if args.get(1).map(String::as_str) == Some("report") => {
            let result = audit::load_report("research/phase2_basket_audit.json")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&result).map_err(|error| error.to_string())?
            );
            Ok(())
        }
        Some("audit") if args.get(1).map(String::as_str) == Some("refresh") => {
            print(&audit::refresh_saved_report(
                &settings.collection.basket_path,
                "research/phase2_basket_audit.json",
            )?)
        }
        Some("audit") if args.get(1).map(String::as_str) == Some("alternative") => {
            let result =
                audit::run_live_for_path(&settings, "research/phase2_basket_alternative.toml")?;
            fs::write(
                "research/phase2_basket_alternative_audit.json",
                format!(
                    "{}\n",
                    serde_json::to_string_pretty(&result).map_err(|error| error.to_string())?
                ),
            )
            .map_err(|error| error.to_string())?;
            print(&result)
        }
        Some("basket") if args.get(1).map(String::as_str) == Some("compare") => {
            compare_existing_baskets(&settings)
        }
        Some("basket") if args.get(1).map(String::as_str) == Some("approve") => {
            let requested_id = argument(args, "--id").ok_or("укажите точный ID через --id")?;
            let mut database = database(&settings.collection.database_path)?;
            print(&phase21::approve(&mut database, &settings, requested_id)?)
        }
        Some("basket") if args.get(1).map(String::as_str) == Some("active") => {
            let database = database(&settings.collection.database_path)?;
            print(&phase21::active(&database)?.unwrap_or_else(
                || json!({"status":"not_set","fallback_path":settings.collection.basket_path}),
            ))
        }
        Some("basket") if args.get(1).map(String::as_str) == Some("versions") => {
            let database = database(&settings.collection.database_path)?;
            print(&phase21::versions(&database)?)
        }
        Some("baseline") if args.get(1).map(String::as_str) == Some("create") => {
            let mut database = database(&settings.collection.database_path)?;
            let active_settings = phase21::active_settings(&settings, &database)?;
            let active_basket = basket::load(&active_settings.collection.basket_path)?;
            if phase21::active(&database)?.is_some() {
                print(&phase21::baseline_status_for(
                    &database,
                    &active_basket.basket_id,
                )?)
            } else {
                let audit = audit::load_report(phase21::audit_path(&active_settings))?;
                print(&weekly::create_baseline(
                    &mut database,
                    &active_settings,
                    &audit,
                )?)
            }
        }
        Some("baseline") if args.get(1).map(String::as_str) == Some("status") => {
            let database = database(&settings.collection.database_path)?;
            let active_settings = phase21::active_settings(&settings, &database)?;
            let active_basket = basket::load(&active_settings.collection.basket_path)?;
            print(&phase21::baseline_status_for(
                &database,
                &active_basket.basket_id,
            )?)
        }
        Some("collect") if args.get(1).map(String::as_str) == Some("weekly") => {
            let mut database = database(&settings.collection.database_path)?;
            let active_settings = phase21::active_settings(&settings, &database)?;
            let audit = audit::load_report(phase21::audit_path(&active_settings))?;
            let revision = args
                .iter()
                .position(|value| value == "--revision")
                .and_then(|index| args.get(index + 1))
                .map(String::as_str);
            let report = weekly::collect(&active_settings, &mut database, &audit, revision)?;
            print(&report)
        }
        Some("weeks") => {
            let database = database(&settings.collection.database_path)?;
            let requested = argument(args, "--id");
            let selected = if requested.is_some() {
                requested.map(str::to_owned)
            } else {
                phase21::active(&database)?
                    .and_then(|value| value["basket_id"].as_str().map(str::to_owned))
            };
            print(&weekly::list_weeks(&database, selected.as_deref())?)
        }
        Some("report") if args.get(1).map(String::as_str) == Some("weekly") => {
            let database = database(&settings.collection.database_path)?;
            print(&weekly::refresh_exports(&database)?)
        }
        Some("report") if args.get(1).map(String::as_str) == Some("status") => {
            let database = database(&settings.collection.database_path)?;
            print(&weekly::phase2_stats(&database)?)
        }
        Some("report") if args.get(1).map(String::as_str) == Some("inflation") => {
            let database = database(&settings.collection.database_path)?;
            print(&phase21::inflation(&database, argument(args, "--id"))?)
        }
        Some("report")
            if matches!(
                args.get(1).map(String::as_str),
                Some("products" | "categories" | "promotions")
            ) =>
        {
            report_status(args.get(1).map(String::as_str).unwrap())
        }
        Some("schedule") if args.get(1).map(String::as_str) == Some("export") => {
            let day = args.get(2).map(String::as_str).unwrap_or("monday");
            let time = args.get(3).map(String::as_str).unwrap_or("09:00");
            print(&scheduler::export_definition(
                env!("CARGO_MANIFEST_DIR"),
                day,
                time,
            )?)
        }
        Some("schedule") if args.get(1).map(String::as_str) == Some("install") => {
            let day = args.get(2).map(String::as_str).unwrap_or("monday");
            let time = args.get(3).map(String::as_str).unwrap_or("09:00");
            print(&scheduler::install(env!("CARGO_MANIFEST_DIR"), day, time)?)
        }
        Some("schedule") if args.get(1).map(String::as_str) == Some("status") => {
            print(&scheduler::status()?)
        }
        Some("schedule") if args.get(1).map(String::as_str) == Some("remove") => {
            print(&scheduler::remove()?)
        }
        Some(command) => Err(format!("команда Phase 2 пока не реализована: {command}")),
        None => Err("не указана команда Phase 2".to_owned()),
    }
}

#[allow(dead_code)]
fn compare_baskets(settings: &crate::domain::Settings) -> Result<(), String> {
    let original = basket::load(&settings.collection.basket_path)?;
    let audit_report = audit::load_report("research/phase2_basket_audit.json")?;
    let excluded = audit_report
        .items
        .iter()
        .filter(|item| !item.suitable_for_fixed_index)
        .map(|item| item.source_product_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let catalog = load_catalog(&settings.collection.products_path)?;
    let mut alternative =
        basket::optimize_balanced(&catalog, &settings.budget.calculate()?, &excluded)?;
    alternative.basket_id = format!(
        "basket-phase2-balanced-{}",
        Utc::now().format("%Y%m%dT%H%M%SZ")
    );
    alternative.status = "draft".to_owned();
    alternative.approved_at.clear();
    basket::save("research/phase2_basket_alternative.toml", &alternative)?;
    let original_concentration = audit::concentration(&original)?;
    let alternative_concentration = audit::concentration(&alternative)?;
    let markdown = format!(
        "# Сравнение корзин Phase 2\n\n| Метрика | Исходная A | Сбалансированная B |\n|---|---:|---:|\n| Статус | approved | draft |\n| Групп | {} | {} |\n| Стоимость, ₽ | {} | {} |\n| Top-4, % | {} | {} |\n| Top-10, % | {} | {} |\n| HHI | {} | {} |\n\nАльтернатива не утверждена и не изменяет `config/basket.toml`. Исключены позиции, не прошедшие аудит пригодности фиксированного физического индекса: {}.\n",
        original.items.len(),
        alternative.items.len(),
        original.total_rub,
        alternative.total_rub,
        original_concentration.top_four_share,
        alternative_concentration.top_four_share,
        original_concentration.top_ten_share,
        alternative_concentration.top_ten_share,
        original_concentration.hhi,
        alternative_concentration.hhi,
        excluded.into_iter().collect::<Vec<_>>().join(", ")
    );
    fs::write("research/phase2_basket_comparison.md", markdown)
        .map_err(|error| error.to_string())?;
    print(&json!({
        "original": {"id":original.basket_id,"groups":original.items.len(),"total":original.total_rub,"concentration":original_concentration},
        "alternative": {"id":alternative.basket_id,"groups":alternative.items.len(),"total":alternative.total_rub,"concentration":alternative_concentration,"status":"draft"},
        "original_unchanged_sha256": audit::sha256_file(&settings.collection.basket_path)?
    }))
}

fn compare_existing_baskets(_settings: &crate::domain::Settings) -> Result<(), String> {
    let original = basket::load("config/basket.toml")?;
    let alternative = basket::load(phase21::DRAFT_PATH)?;
    let original_concentration = audit::concentration(&original)?;
    let alternative_concentration = audit::concentration(&alternative)?;
    print(&json!({
        "original":{"id":original.basket_id,"total":original.total_rub,"groups":original.items.len(),"concentration":original_concentration,"sha256":audit::sha256_file("config/basket.toml")?},
        "agreed_alternative":{"id":alternative.basket_id,"total":alternative.total_rub,"groups":alternative.items.len(),"concentration":alternative_concentration,"sha256":audit::sha256_file(phase21::DRAFT_PATH)?}
    }))
}

fn argument<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|value| value == name)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

fn database(path: &str) -> Result<Database, String> {
    let mut database = Database::open(path)?;
    database.migrate()?;
    Ok(database)
}

fn print<T: serde::Serialize>(value: &T) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn report_status(kind: &str) -> Result<(), String> {
    let weekly = weekly::load_weekly_report("output/weekly_report.json")?;
    let value = match kind {
        "inflation" => {
            json!({"status":"BlockedByBasketAudit","full_index":weekly.full_index,"comparable_subset_index":weekly.comparable_subset_index,"warning":"одна календарная неделя и неподтверждённая масса банана; динамика не публикуется"})
        }
        "products" => {
            json!({"period":format!("{}-W{:02}",weekly.iso_year,weekly.iso_week),"observations":weekly.observations})
        }
        "categories" => json!({"status":"UnavailableWithoutBaseline","categories":[]}),
        "promotions" => {
            json!({"period":format!("{}-W{:02}",weekly.iso_year,weekly.iso_week),"events":weekly.observations.into_iter().filter(|item| item.old_price.is_some() || item.discount_info.is_some()).collect::<Vec<_>>() })
        }
        _ => unreachable!(),
    };
    print(&value)
}

fn backup(database_path: &str) -> Result<(), String> {
    let directory = "data/backups";
    fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let path = format!(
        "{directory}/phase1-pre-phase2-{}.sqlite3",
        Utc::now().format("%Y%m%dT%H%M%SZ")
    );
    Database::backup(database_path, &path)?;
    let checksum = audit::sha256_file(&path)?;
    let backup_database = Database::open(&path)?;
    let metadata_path = format!("{path}.metadata.json");
    let metadata = json!({
        "created_at": Utc::now().to_rfc3339(),
        "database_path": path,
        "sha256": checksum,
        "integrity": backup_database.integrity_check()?,
        "foreign_key_check": backup_database.foreign_key_check()?,
        "migration_versions": backup_database.migration_versions()?,
        "stats": backup_database.stats()?,
        "phase2_stats": weekly::phase2_stats(&backup_database)?
    });
    fs::write(
        &metadata_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&metadata).map_err(|error| error.to_string())?
        ),
    )
    .map_err(|error| error.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "status": "ok",
            "path": path,
            "sha256": checksum,
            "integrity": "ok",
            "metadata_path": metadata_path
        }))
        .map_err(|error| error.to_string())?
    );
    Ok(())
}
