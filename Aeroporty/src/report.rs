use chrono::Utc;

use crate::models::{
    Airport, AttributeStatistics, Conflict, Evidence, EvidenceMethod, GeographicIssue,
    IssueSeverity, ProcessingStats, QuarantineCategory, QuarantineRecord, RejectedAirport, Report,
    SourceInfo,
};

pub struct ReportMetadata {
    pub sources: Vec<SourceInfo>,
    pub conflicts: Vec<Conflict>,
    pub recovered_records: usize,
    pub recovered_attributes: usize,
    pub timezone_dataset_version: String,
    pub quarantine: Vec<QuarantineRecord>,
    pub geographic_issues: Vec<GeographicIssue>,
    pub faa: Option<crate::models::FaaStatistics>,
    pub phase3: Option<crate::models::Phase3Statistics>,
    pub anac: Option<crate::models::AnacStatistics>,
    pub phase4: Option<crate::models::Phase4Statistics>,
    pub phase5: Option<crate::models::Phase5Statistics>,
}

pub fn build_report(
    airports: &[Airport],
    rejected: &[RejectedAirport],
    mut stats: ProcessingStats,
    metadata: ReportMetadata,
) -> Report {
    stats.accepted = airports.len();
    stats.rejected = rejected.len();
    stats.recovered_records = metadata.recovered_records;
    stats.recovered_attributes = metadata.recovered_attributes;
    stats.conflicts = metadata.conflicts.len();
    stats.physical_runways = 0;
    stats.runway_rows.included_in_dataset = 0;
    stats.attributes = AttributeStatistics::default();
    stats.by_country.clear();
    stats.by_type.clear();
    stats.rejection_reasons.clear();

    for airport in airports {
        stats.physical_runways += airport.runways.len();
        stats.runway_rows.included_in_dataset += airport.runways.len();
        *stats.by_country.entry(airport.country.clone()).or_default() += 1;
        *stats
            .by_type
            .entry(airport.airport_type.clone())
            .or_default() += 1;
        count_airport_attributes(airport, &mut stats.attributes);
    }
    for airport in rejected {
        if airport
            .reasons
            .iter()
            .any(|reason| reason.contains("статус open/close"))
        {
            stats.uncertain_status_records += 1;
        }
        for reason in &airport.reasons {
            *stats.rejection_reasons.entry(reason.clone()).or_default() += 1;
        }
    }
    stats.geographic_warnings = metadata
        .geographic_issues
        .iter()
        .filter(|issue| issue.severity == IssueSeverity::Warning)
        .count();
    stats.geographic_errors = metadata
        .geographic_issues
        .iter()
        .filter(|issue| issue.severity == IssueSeverity::Error)
        .count();
    stats.runway_rows.quarantined = metadata
        .quarantine
        .iter()
        .filter(|row| {
            matches!(
                row.category,
                QuarantineCategory::Waterway
                    | QuarantineCategory::Helipad
                    | QuarantineCategory::UnknownType
                    | QuarantineCategory::InvalidRecord
                    | QuarantineCategory::IncompleteLandRunway
                    | QuarantineCategory::Duplicate
                    | QuarantineCategory::GeographicConflict
                    | QuarantineCategory::MalformedCsv
                    | QuarantineCategory::InvalidReference
            )
        })
        .count();
    stats.runway_rows.invalid_or_duplicate = metadata
        .quarantine
        .iter()
        .filter(|row| {
            matches!(
                row.category,
                QuarantineCategory::InvalidRecord | QuarantineCategory::Duplicate
            )
        })
        .count();
    stats.malformed_source_rows = metadata
        .quarantine
        .iter()
        .filter(|row| row.category == QuarantineCategory::MalformedCsv)
        .count();

    Report {
        generated_at: Utc::now().to_rfc3339(), sources: metadata.sources, statistics: stats,
        conflicts: metadata.conflicts,
        limitations: vec![
            "FAA/NASR применяется только к объектам выпуска и регионального охвата FAA; остальной мировой набор по-прежнему основан на OurAirports.".into(),
            "FAA surface и OurAirports surface при разных классификациях/датах помечаются NotComparable, а не автоматически перезаписываются.".into(),
            "status=open означает: объект известного типа не помечен OurAirports как постоянно закрытый на дату источника; это не доступность для рейса сейчас, не NOTAM и не наличие регулярных рейсов.".into(),
            "Строгий экспорт содержит только классифицированные наземные ВПП; waterways, helipads, unknown и повреждённые строки находятся в quarantine.csv.".into(),
            "Метрики evidence относятся к обязательным значениям строгого датасета; отклонённые записи и карантин считаются отдельно.".into(),
            "ANAC status=open означает наличие действующей записи в актуальном реестре, а не текущую эксплуатационную доступность или отсутствие NOTAM.".into(),
            "Устаревшие выгрузки ANAC Características Gerais (2023) и Pistas (2024) сохранены и проверены, но не используются для восстановления актуальных обязательных значений.".into(),
        ],
        timezone_dataset_version: metadata.timezone_dataset_version,
        faa: metadata.faa,
        phase3: metadata.phase3,
        anac: metadata.anac,
        phase4: metadata.phase4,
        phase5: metadata.phase5,
    }
}

fn count_airport_attributes(airport: &Airport, stats: &mut AttributeStatistics) {
    let airport_fields = [
        "icao",
        "latitude",
        "longitude",
        "status",
        "type",
        "country",
        "elevation_ft",
        "timezone",
        "runways_count",
        "id",
        "name",
    ];
    for field in airport_fields {
        count_value(airport.evidence.get(field), stats);
    }
    for runway in &airport.runways {
        for field in ["id", "length_ft", "surface"] {
            count_value(runway.evidence.get(field), stats);
        }
    }
}

fn count_value(evidence: Option<&Vec<Evidence>>, stats: &mut AttributeStatistics) {
    stats.filled += 1;
    stats.structurally_valid += 1;
    let Some(items) = evidence else {
        stats.unresolved += 1;
        return;
    };
    if items
        .iter()
        .any(|item| item.method == EvidenceMethod::Conflicting)
    {
        stats.conflicting += 1;
        return;
    }
    if items
        .iter()
        .any(|item| item.method == EvidenceMethod::IndependentlyVerified)
    {
        stats.independently_verified += 1;
    } else if items
        .iter()
        .any(|item| item.method == EvidenceMethod::Recovered)
    {
        stats.source_reported += 1;
    } else if items
        .iter()
        .any(|item| item.method == EvidenceMethod::Derived)
    {
        stats.derived += 1;
    } else if items
        .iter()
        .any(|item| item.method == EvidenceMethod::SourceReported)
    {
        stats.source_reported += 1;
    } else {
        stats.unresolved += 1;
    }
}

pub fn print_stats(stats: &ProcessingStats) {
    println!("Исходных аэропортов: {}", stats.source_airports);
    println!("С ICAO: {}", stats.airports_with_icao);
    println!("Принято: {}", stats.accepted);
    println!("Исключено: {}", stats.rejected);
    println!(
        "Наземных физических ВПП в строгом датасете: {}",
        stats.physical_runways
    );
    println!("Строк ВПП источника: {}", stats.runway_rows.source_rows);
    println!(
        "Результаты в ICAO-области: land={}, waterway={}, helipad={}, unknown={}, invalid/duplicate={} (duplicate может пересекаться с land); вне ICAO-области={}",
        stats.runway_rows.land_runways,
        stats.runway_rows.waterways,
        stats.runway_rows.helipads,
        stats.runway_rows.unknown,
        stats.runway_rows.invalid_or_duplicate,
        stats.runway_rows.not_classified_outside_icao_scope
    );
    println!("Карантинных строк: {}", stats.runway_rows.quarantined);
    println!(
        "Обязательных значений: filled={}, structurally_valid={}, source_reported={}, derived={}, independently_verified={}, conflicting={}, unresolved={}",
        stats.attributes.filled,
        stats.attributes.structurally_valid,
        stats.attributes.source_reported,
        stats.attributes.derived,
        stats.attributes.independently_verified,
        stats.attributes.conflicting,
        stats.attributes.unresolved
    );
    println!(
        "Географические проверки: warnings={}, errors={}",
        stats.geographic_warnings, stats.geographic_errors
    );
    println!("Восстановлено записей: {}", stats.recovered_records);
    println!("Восстановлено атрибутов: {}", stats.recovered_attributes);
    println!("Конфликтов источников: {}", stats.conflicts);
    if !stats.rejection_reasons.is_empty() {
        println!("Основные причины исключения:");
        let mut reasons: Vec<_> = stats.rejection_reasons.iter().collect();
        reasons.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
        for (reason, count) in reasons.into_iter().take(10) {
            println!("  {count:>6} — {reason}");
        }
    }
}
