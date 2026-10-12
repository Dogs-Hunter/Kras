use std::{collections::HashMap, path::Path};

use crate::{
    error::Result,
    models::{
        AirportDraft, AirportStatus, Evidence, EvidenceMap, ProcessingStats, QuarantineCategory,
        QuarantineRecord, RunwayDraft, RunwayKind,
    },
    runway_classification::classify_runway,
    sources::ourairports::{RawAirport, RawCountry, RawRunway},
};

type ParsedRunways = (HashMap<u64, Vec<RunwayDraft>>, Vec<QuarantineRecord>, usize);

pub fn parse_ourairports(
    cache_dir: &Path,
) -> Result<(Vec<AirportDraft>, ProcessingStats, Vec<QuarantineRecord>)> {
    let (countries, mut quarantine) = parse_countries(&cache_dir.join("countries.csv"))?;
    let (mut runways, runway_quarantine, runway_rows) =
        parse_runways(&cache_dir.join("runways.csv"))?;
    quarantine.extend(runway_quarantine);

    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_path(cache_dir.join("airports.csv"))?;
    let mut drafts = Vec::new();
    let mut stats = ProcessingStats::default();
    stats.runway_rows.source_rows = runway_rows;

    for row in reader.deserialize::<RawAirport>() {
        stats.source_airports += 1;
        let raw = match row {
            Ok(raw) => raw,
            Err(error) => {
                stats.malformed_source_rows += 1;
                quarantine.push(csv_quarantine("airports.csv", &error));
                continue;
            }
        };
        if raw.icao_code.trim().is_empty() {
            continue;
        }
        stats.airports_with_icao += 1;
        let id = parse_u64(&raw.id);
        let record_id = raw.id.trim().to_owned();
        let icao = raw.icao_code.trim().to_ascii_uppercase();
        let mut airport_runways = id
            .and_then(|value| runways.remove(&value))
            .unwrap_or_default();
        let mut airport_quarantine = Vec::new();
        for runway in &mut airport_runways {
            let (kind, reason) = classify_runway(&raw.airport_type, runway);
            runway.kind = kind;
            runway.classification_reason = reason.clone();
            runway
                .evidence
                .entry("kind".into())
                .or_default()
                .push(Evidence::derived(
                    "Aeroporty runway classifier v1",
                    runway.id.clone(),
                    format!(
                        "type={};surface={:?};ends={:?}/{:?}",
                        raw.airport_type, runway.surface, runway.le_ident, runway.he_ident
                    ),
                    format!("{kind:?}"),
                    reason.clone(),
                ));
        }
        let mut land_runways = Vec::new();
        for runway in airport_runways {
            match runway.kind {
                RunwayKind::LandRunway => {
                    stats.runway_rows.land_runways += 1;
                    land_runways.push(runway);
                }
                RunwayKind::Waterway => {
                    stats.runway_rows.waterways += 1;
                    airport_quarantine.push(classification_quarantine(
                        &icao,
                        id,
                        &runway,
                        QuarantineCategory::Waterway,
                        false,
                    ));
                }
                RunwayKind::Helipad => {
                    stats.runway_rows.helipads += 1;
                    airport_quarantine.push(classification_quarantine(
                        &icao,
                        id,
                        &runway,
                        QuarantineCategory::Helipad,
                        false,
                    ));
                }
                RunwayKind::Unknown => {
                    stats.runway_rows.unknown += 1;
                    airport_quarantine.push(classification_quarantine(
                        &icao,
                        id,
                        &runway,
                        QuarantineCategory::UnknownType,
                        true,
                    ));
                }
                RunwayKind::Invalid => {
                    stats.runway_rows.invalid_or_duplicate += 1;
                    airport_quarantine.push(classification_quarantine(
                        &icao,
                        id,
                        &runway,
                        QuarantineCategory::InvalidRecord,
                        false,
                    ));
                }
            }
        }

        let latitude = parse_f64(&raw.latitude_deg);
        let longitude = parse_f64(&raw.longitude_deg);
        let elevation_ft = parse_i32(&raw.elevation_ft);
        let country = countries.get(raw.iso_country.trim()).cloned();
        let (status, status_evidence) = status_from_type(&raw.airport_type, &record_id);
        let mut evidence = EvidenceMap::new();
        add_source(
            &mut evidence,
            "icao",
            &record_id,
            &raw.icao_code,
            &icao,
            "формат проверяется валидатором",
        );
        add_source(
            &mut evidence,
            "latitude",
            &record_id,
            &raw.latitude_deg,
            &option_string(latitude),
            "значение непосредственно из источника",
        );
        add_source(
            &mut evidence,
            "longitude",
            &record_id,
            &raw.longitude_deg,
            &option_string(longitude),
            "значение непосредственно из источника",
        );
        add_source(
            &mut evidence,
            "type",
            &record_id,
            &raw.airport_type,
            &raw.airport_type,
            "значение непосредственно из источника",
        );
        add_source(
            &mut evidence,
            "elevation_ft",
            &record_id,
            &raw.elevation_ft,
            &elevation_ft.map(|v| v.to_string()).unwrap_or_default(),
            "единица источника — ft; синтаксический разбор без подмены",
        );
        add_source(
            &mut evidence,
            "id",
            &record_id,
            &raw.id,
            &id.map(|v| v.to_string()).unwrap_or_default(),
            "числовой ID источника",
        );
        add_source(
            &mut evidence,
            "name",
            &record_id,
            &raw.name,
            raw.name.trim(),
            "непустое значение проверяется валидатором",
        );
        evidence
            .entry("country".into())
            .or_default()
            .push(Evidence::derived(
                "OurAirports countries.csv",
                record_id.clone(),
                raw.iso_country.clone(),
                country.clone().unwrap_or_default(),
                "join iso_country -> countries.code; это согласование внутри одного источника",
            ));
        evidence
            .entry("status".into())
            .or_default()
            .push(status_evidence);

        let mut provenance = std::collections::BTreeMap::new();
        provenance.insert("record".into(), "OurAirports airports.csv".into());
        provenance.insert(
            "icao".into(),
            format!("OurAirports icao_code (ident={})", raw.ident),
        );
        drafts.push(AirportDraft {
            icao,
            latitude,
            longitude,
            status,
            airport_type: raw.airport_type,
            country,
            elevation_ft,
            timezone: None,
            timezone_resolution: Default::default(),
            id,
            name: raw.name.trim().to_owned(),
            runways: land_runways,
            quarantine: airport_quarantine,
            geographic_issues: Vec::new(),
            provenance,
            evidence,
        });
    }
    let classified = stats.runway_rows.land_runways
        + stats.runway_rows.waterways
        + stats.runway_rows.helipads
        + stats.runway_rows.unknown
        + stats.runway_rows.invalid_or_duplicate;
    stats.runway_rows.not_classified_outside_icao_scope = runway_rows.saturating_sub(classified);
    stats.runway_rows.quarantined = quarantine.len()
        + drafts
            .iter()
            .map(|airport| airport.quarantine.len())
            .sum::<usize>();
    Ok((drafts, stats, quarantine))
}

fn parse_countries(path: &Path) -> Result<(HashMap<String, String>, Vec<QuarantineRecord>)> {
    let mut reader = csv::ReaderBuilder::new().flexible(true).from_path(path)?;
    let mut countries = HashMap::new();
    let mut quarantine = Vec::new();
    for row in reader.deserialize::<RawCountry>() {
        match row {
            Ok(raw) if !raw.code.trim().is_empty() && !raw.name.trim().is_empty() => {
                countries.insert(raw.code, raw.name);
            }
            Ok(raw) => quarantine.push(QuarantineRecord {
                source: "countries.csv".into(),
                row_number: None,
                airport_id: None,
                icao: None,
                record_id: Some(raw.code),
                category: QuarantineCategory::InvalidRecord,
                reason: "пустой код или название страны".into(),
                affects_airport: false,
                raw: None,
            }),
            Err(error) => quarantine.push(csv_quarantine("countries.csv", &error)),
        }
    }
    Ok((countries, quarantine))
}

fn parse_runways(path: &Path) -> Result<ParsedRunways> {
    let mut reader = csv::ReaderBuilder::new().flexible(true).from_path(path)?;
    let mut by_airport: HashMap<u64, Vec<RunwayDraft>> = HashMap::new();
    let mut quarantine = Vec::new();
    let mut source_rows = 0;
    for row in reader.deserialize::<RawRunway>() {
        source_rows += 1;
        let raw = match row {
            Ok(raw) => raw,
            Err(error) => {
                quarantine.push(csv_quarantine("runways.csv", &error));
                continue;
            }
        };
        let Some(airport_id) = parse_u64(&raw.airport_ref) else {
            quarantine.push(QuarantineRecord {
                source: "OurAirports runways.csv".into(),
                row_number: None,
                airport_id: None,
                icao: nonempty(&raw.airport_ident),
                record_id: nonempty(&raw.id),
                category: QuarantineCategory::InvalidReference,
                reason: "airport_ref не является числовым идентификатором".into(),
                affects_airport: true,
                raw: Some(format!("airport_ref={}", raw.airport_ref)),
            });
            continue;
        };
        let id = raw.id.trim().to_owned();
        let length_ft = parse_f64(&raw.length_ft);
        let width_ft = parse_f64(&raw.width_ft);
        let surface = nonempty(&raw.surface);
        let mut evidence = EvidenceMap::new();
        add_source(&mut evidence, "id", &id, &raw.id, &id, "ID исходной строки");
        add_source(
            &mut evidence,
            "length_ft",
            &id,
            &raw.length_ft,
            &option_string(length_ft),
            "единица источника — ft",
        );
        add_source(
            &mut evidence,
            "surface",
            &id,
            &raw.surface,
            surface.as_deref().unwrap_or(""),
            "неконтролируемый словарь OurAirports; не является независимой проверкой",
        );
        let mut provenance = std::collections::BTreeMap::new();
        provenance.insert("record".into(), "OurAirports runways.csv".into());
        by_airport.entry(airport_id).or_default().push(RunwayDraft {
            id,
            length_ft,
            width_ft,
            surface,
            le_ident: nonempty(&raw.le_ident),
            he_ident: nonempty(&raw.he_ident),
            le_latitude: parse_f64(&raw.le_latitude_deg),
            le_longitude: parse_f64(&raw.le_longitude_deg),
            he_latitude: parse_f64(&raw.he_latitude_deg),
            he_longitude: parse_f64(&raw.he_longitude_deg),
            closed: matches!(raw.closed.trim(), "1" | "true" | "TRUE"),
            kind: RunwayKind::Unknown,
            classification_reason: String::new(),
            provenance,
            evidence,
        });
    }
    Ok((by_airport, quarantine, source_rows))
}

fn status_from_type(airport_type: &str, record_id: &str) -> (Option<AirportStatus>, Evidence) {
    match airport_type {
        "closed" | "closed_airport" => (
            Some(AirportStatus::Close),
            Evidence::derived(
                "OurAirports airports.csv",
                record_id,
                airport_type,
                "close",
                "тип closed/closed_airport интерпретируется как постоянное закрытие на дату классификации источника; не NOTAM",
            ),
        ),
        "small_airport" | "medium_airport" | "large_airport" | "heliport" | "seaplane_base" => (
            Some(AirportStatus::Open),
            Evidence::derived(
                "OurAirports airports.csv",
                record_id,
                airport_type,
                "open",
                "политика Phase 1: известный не-closed тип означает существующий, не помеченный постоянно закрытым объект на дату источника; не подтверждает доступность сейчас или регулярные рейсы",
            ),
        ),
        other => (
            None,
            Evidence::unresolved(
                "OurAirports airports.csv",
                record_id,
                other,
                "тип не даёт достаточного основания для open/close",
            ),
        ),
    }
}

fn classification_quarantine(
    icao: &str,
    airport_id: Option<u64>,
    runway: &RunwayDraft,
    category: QuarantineCategory,
    affects_airport: bool,
) -> QuarantineRecord {
    QuarantineRecord {
        source: "OurAirports runways.csv".into(),
        row_number: None,
        airport_id,
        icao: Some(icao.into()),
        record_id: Some(runway.id.clone()),
        category,
        reason: runway.classification_reason.clone(),
        affects_airport,
        raw: Some(format!(
            "surface={:?}; ends={:?}/{:?}; length_ft={:?}; width_ft={:?}",
            runway.surface, runway.le_ident, runway.he_ident, runway.length_ft, runway.width_ft
        )),
    }
}

fn csv_quarantine(source: &str, error: &csv::Error) -> QuarantineRecord {
    QuarantineRecord {
        source: source.into(),
        row_number: error.position().map(|p| p.line()),
        airport_id: None,
        icao: None,
        record_id: None,
        category: QuarantineCategory::MalformedCsv,
        reason: error.to_string(),
        affects_airport: true,
        raw: None,
    }
}

fn add_source(
    evidence: &mut EvidenceMap,
    field: &str,
    record_id: &str,
    raw: &str,
    normalized: &str,
    validation: &str,
) {
    evidence
        .entry(field.into())
        .or_default()
        .push(Evidence::source_reported(
            "OurAirports",
            record_id,
            raw,
            normalized,
            validation,
        ));
}

fn option_string<T: ToString>(value: Option<T>) -> String {
    value.map(|v| v.to_string()).unwrap_or_default()
}
fn parse_f64(value: &str) -> Option<f64> {
    value.trim().parse().ok()
}
fn parse_i32(value: &str) -> Option<i32> {
    value.trim().parse().ok()
}
fn parse_u64(value: &str) -> Option<u64> {
    value.trim().parse().ok()
}
fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}
