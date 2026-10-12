use std::path::Path;

use crate::{
    config::Config,
    download,
    enrichment::enrich_from_local,
    error::{Error, Result, io_error},
    export::{ExportFormat, export_dataset, read_dataset_json, verify_exports, write_reports},
    models::{Conflict, Dataset, Report},
    parser::parse_ourairports,
    report::{ReportMetadata, build_report},
    timezone::TimezoneResolver,
    validation::{validate_airport, validate_all},
};

pub async fn run(config: &Config, formats: &[ExportFormat]) -> Result<Report> {
    ensure_paths(config)?;
    let sources = download::cached_sources(config).await?;
    let mut sources = sources;
    let (mut drafts, stats, mut parse_quarantine) = parse_ourairports(&config.cache_dir)?;
    let mut faa_result = if config.enable_faa {
        let (faa_dataset, source) = crate::faa::prepare(config).await?;
        sources.push(source);
        let mut result = crate::faa::apply(&mut drafts, &faa_dataset);
        crate::faa::add_new_airports(&mut drafts, &faa_dataset, &mut result);
        Some(result)
    } else {
        None
    };
    let mut anac_result = if config.enable_anac {
        let (anac_dataset, anac_sources) = crate::anac::prepare(config).await?;
        sources.extend(anac_sources);
        Some(crate::anac::apply(&mut drafts, &anac_dataset))
    } else {
        None
    };
    let phase5_resolutions = if config.enable_phase5 {
        crate::phase5::apply_conflict_resolutions(
            &config.root,
            &mut drafts,
            faa_result.as_mut(),
            anac_result.as_mut(),
        )?
    } else {
        Vec::new()
    };
    let timezone = TimezoneResolver::new();
    timezone.enrich(&mut drafts);
    let enrichment = if config.enable_local_enrichment {
        enrich_from_local(&mut drafts, &config.enrichment_file)?
    } else {
        Default::default()
    };
    let validation = validate_all(drafts);
    parse_quarantine.extend(validation.quarantine.clone());
    let dataset = Dataset {
        airports: validation.airports,
    };
    let mut phase3 = crate::phase3::build(config, &dataset, &validation.rejected)?;
    phase3.statistics.unresolved_conflicts = faa_result
        .as_ref()
        .map_or(0, |v| v.statistics.unresolved_conflicts);
    if let Some(faa) = &mut faa_result {
        let phase1_path = config
            .root
            .join("output")
            .join("candidate-phase1")
            .join("airports.json");
        if phase1_path.exists() {
            let phase1 = read_dataset_json(&phase1_path)?;
            let before: std::collections::HashSet<_> =
                phase1.airports.iter().map(|a| a.icao.as_str()).collect();
            let after: std::collections::HashSet<_> =
                dataset.airports.iter().map(|a| a.icao.as_str()).collect();
            faa.statistics.recovered_airports = dataset
                .airports
                .iter()
                .filter(|a| {
                    !before.contains(a.icao.as_str())
                        && !a
                            .provenance
                            .get("source")
                            .is_some_and(|v| v == "FAA/NASR APT_BASE")
                })
                .count();
            faa.statistics.new_exclusions = before.difference(&after).count();
        }
    }
    let phase4_current = if let Some(anac) = &mut anac_result {
        Some(crate::phase4::build(
            config,
            &dataset,
            &validation.rejected,
            anac,
        )?)
    } else {
        None
    };
    let mut report_conflicts = enrichment.conflicts.clone();
    if let Some(faa) = &faa_result {
        report_conflicts.extend(
            faa.conflicts
                .iter()
                .filter(|row| row.decision == "Conflict")
                .map(|row| Conflict {
                    icao: row.icao.clone(),
                    field: row.runway_id.as_ref().map_or_else(
                        || row.field.clone(),
                        |id| format!("runway {id} {}", row.field),
                    ),
                    primary_value: row.ourairports_value.clone(),
                    secondary_value: row.faa_value.clone(),
                    secondary_source: "FAA/NASR APT".into(),
                }),
        );
    }
    if let Some(anac) = &anac_result {
        report_conflicts.extend(
            anac.conflicts
                .iter()
                .filter(|row| row.decision == "Conflict")
                .map(|row| Conflict {
                    icao: row.icao.clone(),
                    field: if row.runway.is_empty() {
                        row.field.clone()
                    } else {
                        format!("runway {} {}", row.runway, row.field)
                    },
                    primary_value: row.existing_value.clone(),
                    secondary_value: row.anac_value.clone(),
                    secondary_source: "ANAC".into(),
                }),
        );
    }
    if config.enable_anac {
        let phase3_report: Report =
            read_json(&config.root.join("output/candidate-phase3/report.json"))?;
        if let Some(saved) = phase3_report.phase3 {
            phase3.statistics = saved;
        }
        if let (Some(current), Some(saved)) = (&mut faa_result, phase3_report.faa) {
            current.statistics = saved;
            if config.enable_phase5 {
                current.statistics.automatically_resolved_conflicts = 7;
                current.statistics.unresolved_conflicts = 0;
            }
        }
    }
    let (saved_phase4, prior_recovered_records) = if config.enable_phase5 {
        let report: Report = read_json(&config.root.join("output/candidate-phase4/report.json"))?;
        (report.phase4, report.statistics.recovered_records)
    } else {
        (None, 0)
    };
    if config.enable_phase5
        && let (Some(anac), Some(phase4)) = (&mut anac_result, &saved_phase4)
    {
        anac.statistics.restored_airports = phase4.restored_phase3_exclusions;
    }
    let phase5 = if config.enable_phase5 {
        let anac = anac_result.as_ref().ok_or_else(|| {
            Error::Data(
                "Phase 5 requires the ANAC adapter; remove --no-anac or use --no-phase5".into(),
            )
        })?;
        Some(crate::phase5::build(
            &config.root,
            &dataset,
            &validation.rejected,
            anac,
            phase5_resolutions,
        )?)
    } else {
        None
    };
    let report = build_report(
        &dataset.airports,
        &validation.rejected,
        stats,
        ReportMetadata {
            sources,
            conflicts: report_conflicts,
            recovered_records: if config.enable_phase5 {
                prior_recovered_records
                    + phase5
                        .as_ref()
                        .map_or(0, |value| value.statistics.restored_conflict_airports)
            } else {
                enrichment.recovered_records
                    + faa_result
                        .as_ref()
                        .map_or(0, |v| v.statistics.recovered_airports)
                    + phase4_current
                        .as_ref()
                        .map_or(0, |v| v.statistics.restored_phase3_exclusions)
            },
            recovered_attributes: enrichment.recovered_attributes
                + faa_result
                    .as_ref()
                    .map_or(0, |v| v.statistics.recovered_values)
                + anac_result
                    .as_ref()
                    .map_or(0, |v| v.statistics.recovered_values),
            timezone_dataset_version: timezone.data_version().to_owned(),
            quarantine: parse_quarantine.clone(),
            geographic_issues: validation.geographic_issues.clone(),
            faa: faa_result.as_ref().map(|value| value.statistics.clone()),
            phase3: Some(phase3.statistics.clone()),
            anac: anac_result.as_ref().map(|value| value.statistics.clone()),
            phase4: saved_phase4.or_else(|| {
                phase4_current
                    .as_ref()
                    .map(|value| value.statistics.clone())
            }),
            phase5: phase5.as_ref().map(|value| value.statistics.clone()),
        },
    );
    export_dataset(&dataset, &config.output_dir, formats)?;
    write_reports(
        &report,
        &validation.rejected,
        &parse_quarantine,
        &validation.geographic_issues,
        &dataset,
        &config.output_dir,
    )?;
    if let Some(faa) = &faa_result {
        crate::faa::write_artifacts(faa, &config.output_dir)?;
        if !config.enable_anac {
            crate::phase3::write(&phase3, faa, &config.output_dir)?;
        }
    }
    if let Some(anac) = &anac_result {
        crate::anac::write_artifacts(anac, &config.output_dir)?;
    }
    if !config.enable_phase5
        && let (Some(anac), Some(phase4)) = (&anac_result, &phase4_current)
    {
        crate::phase4::write(
            phase4,
            anac,
            &config.output_dir,
            &config.root.join("audit/phase4_comparison.md"),
        )?;
    }
    if let Some(phase5) = &phase5 {
        crate::phase5::write(
            phase5,
            &config.output_dir,
            &config.root.join("audit/phase5_comparison.md"),
        )?;
    }
    if formats.len() == ExportFormat::ALL.len() {
        verify_exports(&config.output_dir)?;
    }
    Ok(report)
}

pub async fn update(config: &Config) -> Result<Vec<crate::models::SourceInfo>> {
    ensure_paths(config)?;
    let mut sources = download::update_sources(config).await?;
    if config.enable_faa {
        let (_, source) = crate::faa::prepare(config).await?;
        sources.push(source);
    }
    if config.enable_anac {
        let (_, anac_sources) = crate::anac::prepare(config).await?;
        sources.extend(anac_sources);
    }
    Ok(sources)
}

pub fn export_cached(config: &Config, formats: &[ExportFormat]) -> Result<Report> {
    ensure_paths(config)?;
    let output_dataset = config.output_dir.join("airports.json");
    let dataset = if output_dataset.exists() {
        read_dataset_json(&output_dataset)?
    } else {
        read_dataset_json(&config.cache_dir.join("dataset.json"))?
    };
    let output_report = config.output_dir.join("report.json");
    let report: Report = if output_report.exists() {
        read_json(&output_report)?
    } else {
        read_json(&config.cache_dir.join("report.json"))?
    };
    export_dataset(&dataset, &config.output_dir, formats)?;
    if formats.len() == ExportFormat::ALL.len() {
        verify_exports(&config.output_dir)?;
    }
    Ok(report)
}

pub fn validate_output(config: &Config) -> Result<usize> {
    let dataset = if ExportFormat::ALL.iter().all(|format| {
        let name = match format {
            ExportFormat::Csv => "airports.csv",
            ExportFormat::Json => "airports.json",
            ExportFormat::Toml => "airports.toml",
            ExportFormat::Yaml => "airports.yaml",
        };
        config.output_dir.join(name).exists()
    }) {
        verify_exports(&config.output_dir)?
    } else {
        read_dataset_json(&config.output_dir.join("airports.json"))?
    };
    let mut invalid = Vec::new();
    for airport in &dataset.airports {
        let reasons = validate_airport(airport);
        if !reasons.is_empty() {
            invalid.push(format!("{}: {}", airport.icao, reasons.join("; ")));
        }
    }
    let unique_icao: std::collections::HashSet<_> =
        dataset.airports.iter().map(|a| &a.icao).collect();
    let unique_id: std::collections::HashSet<_> = dataset.airports.iter().map(|a| a.id).collect();
    if unique_icao.len() != dataset.airports.len() {
        invalid.push("в датасете есть повторяющиеся ICAO".into());
    }
    if unique_id.len() != dataset.airports.len() {
        invalid.push("в датасете есть повторяющиеся ID".into());
    }
    if invalid.is_empty() {
        Ok(dataset.airports.len())
    } else {
        Err(Error::Data(format!(
            "валидация не пройдена:\n{}",
            invalid.into_iter().take(50).collect::<Vec<_>>().join("\n")
        )))
    }
}

pub fn read_report(config: &Config) -> Result<Report> {
    let output = config.output_dir.join("report.json");
    if output.exists() {
        read_json(&output)
    } else {
        read_json(&config.cache_dir.join("report.json"))
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = std::fs::read(path).map_err(|e| io_error(path, e))?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn ensure_paths(config: &Config) -> Result<()> {
    std::fs::create_dir_all(&config.cache_dir).map_err(|e| io_error(&config.cache_dir, e))?;
    std::fs::create_dir_all(&config.output_dir).map_err(|e| io_error(&config.output_dir, e))?;
    let root = config
        .root
        .canonicalize()
        .map_err(|e| io_error(&config.root, e))?;
    let output = config
        .output_dir
        .canonicalize()
        .map_err(|e| io_error(&config.output_dir, e))?;
    if !output.starts_with(&root) {
        return Err(Error::Data(format!(
            "директория вывода должна находиться внутри {}",
            root.display()
        )));
    }
    Ok(())
}
