use std::collections::{HashMap, HashSet};

use crate::{
    geo::validate_airport_geography,
    models::{
        Airport, AirportDraft, Evidence, EvidenceMethod, IssueSeverity, QuarantineCategory,
        QuarantineRecord, RejectedAirport, Runway, RunwayDraft, RunwayKind, ValidationOutcome,
    },
    runway_classification::is_unknown_surface,
};

const ALLOWED_TYPES: &[&str] = &[
    "small_airport",
    "medium_airport",
    "large_airport",
    "closed",
    "closed_airport",
    "airport",
];

pub fn validate_all(mut drafts: Vec<AirportDraft>) -> ValidationOutcome {
    let mut icao_counts = HashMap::new();
    let mut id_counts = HashMap::new();
    for draft in &drafts {
        *icao_counts.entry(draft.icao.clone()).or_insert(0usize) += 1;
        if let Some(id) = draft.id {
            *id_counts.entry(id).or_insert(0usize) += 1;
        }
    }

    let mut airports = Vec::new();
    let mut rejected = Vec::new();
    let mut quarantine = Vec::new();
    let mut geographic_issues = Vec::new();
    for mut draft in drafts.drain(..) {
        prepare_runway_validation(&mut draft);
        draft.geographic_issues = validate_airport_geography(&draft);
        for issue in &draft.geographic_issues {
            if issue.severity == IssueSeverity::Error {
                draft.quarantine.push(QuarantineRecord {
                    source: "Aeroporty geographic validation".into(),
                    row_number: None,
                    airport_id: draft.id,
                    icao: Some(draft.icao.clone()),
                    record_id: issue.runway_id.clone(),
                    category: QuarantineCategory::GeographicConflict,
                    reason: issue.message.clone(),
                    affects_airport: true,
                    raw: None,
                });
            }
        }
        let reasons = reasons_for(&draft, &icao_counts, &id_counts);
        geographic_issues.extend(draft.geographic_issues.clone());
        quarantine.extend(draft.quarantine.clone());
        if reasons.is_empty() {
            airports.push(to_airport(draft));
        } else {
            rejected.push(RejectedAirport {
                id: draft.id,
                icao: (!draft.icao.is_empty()).then_some(draft.icao),
                name: draft.name,
                reasons,
            });
        }
    }
    airports.sort_by(|a, b| a.icao.cmp(&b.icao).then(a.id.cmp(&b.id)));
    rejected.sort_by(|a, b| a.icao.cmp(&b.icao).then(a.id.cmp(&b.id)));
    ValidationOutcome {
        airports,
        rejected,
        quarantine,
        geographic_issues,
    }
}

fn prepare_runway_validation(airport: &mut AirportDraft) {
    let mut ids = HashSet::new();
    let mut designations = HashSet::new();
    for runway in &airport.runways {
        let mut reasons = Vec::new();
        if runway.kind != RunwayKind::LandRunway {
            reasons.push("строка в land-runway collection имеет иной тип".to_owned());
        }
        if runway.id.trim().is_empty() {
            reasons.push("нет ID".to_owned());
        }
        if !runway
            .length_ft
            .is_some_and(|value| value.is_finite() && value > 0.0)
        {
            reasons.push("нет положительной длины".to_owned());
        }
        if runway.surface.as_deref().is_none_or(is_unknown_surface) {
            reasons.push("неизвестное покрытие".to_owned());
        }
        if !reasons.is_empty() {
            airport.quarantine.push(QuarantineRecord {
                source: "OurAirports runways.csv".into(),
                row_number: None,
                airport_id: airport.id,
                icao: Some(airport.icao.clone()),
                record_id: Some(runway.id.clone()),
                category: QuarantineCategory::IncompleteLandRunway,
                reason: format!("неполная физическая ВПП: {}", reasons.join(", ")),
                affects_airport: true,
                raw: None,
            });
        }
        if !runway.id.is_empty() && !ids.insert(runway.id.clone()) {
            airport.quarantine.push(duplicate(
                airport,
                runway,
                format!("повтор ID {}", runway.id),
            ));
        }
        if let Some(designation) = designation(runway)
            && !designations.insert(designation.clone())
        {
            airport.quarantine.push(duplicate(
                airport,
                runway,
                format!("повтор физической ВПП {designation}"),
            ));
        }
    }
}

fn duplicate(airport: &AirportDraft, runway: &RunwayDraft, reason: String) -> QuarantineRecord {
    QuarantineRecord {
        source: "OurAirports runways.csv".into(),
        row_number: None,
        airport_id: airport.id,
        icao: Some(airport.icao.clone()),
        record_id: Some(runway.id.clone()),
        category: QuarantineCategory::Duplicate,
        reason,
        affects_airport: true,
        raw: None,
    }
}

fn designation(runway: &RunwayDraft) -> Option<String> {
    let (Some(low), Some(high)) = (&runway.le_ident, &runway.he_ident) else {
        return None;
    };
    if low.trim().is_empty() || high.trim().is_empty() {
        return None;
    }
    let mut ends = [
        low.trim().to_ascii_uppercase(),
        high.trim().to_ascii_uppercase(),
    ];
    ends.sort();
    Some(format!("{}/{}", ends[0], ends[1]))
}

fn reasons_for(
    airport: &AirportDraft,
    icao_counts: &HashMap<String, usize>,
    id_counts: &HashMap<u64, usize>,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if !valid_icao(&airport.icao) {
        reasons.push("некорректный ICAO (ожидаются четыре латинские буквы)".into());
    } else if !has_resolved_evidence(airport.evidence.get("icao")) {
        reasons.push("ICAO не имеет прослеживаемого свидетельства".into());
    }
    if icao_counts.get(&airport.icao).copied().unwrap_or(0) > 1 {
        reasons.push("дублирующийся ICAO".into());
    }
    match airport.id {
        None => reasons.push("отсутствует идентификатор аэропорта".into()),
        Some(id) if id_counts.get(&id).copied().unwrap_or(0) > 1 => {
            reasons.push("дублирующийся идентификатор аэропорта".into())
        }
        _ => {}
    }
    if airport.name.trim().is_empty() {
        reasons.push("отсутствует название аэропорта".into());
    }
    if !ALLOWED_TYPES.contains(&airport.airport_type.as_str()) {
        reasons.push(format!(
            "тип {} не представляет поддерживаемый аэропорт с наземной ВПП",
            airport.airport_type
        ));
    }
    match (airport.latitude, airport.longitude) {
        (Some(lat), Some(lon)) if crate::geo::valid_coordinate(lat, lon) => {}
        _ => reasons.push("некорректные координаты аэропорта".into()),
    }
    if airport
        .country
        .as_deref()
        .is_none_or(|v| v.trim().is_empty())
    {
        reasons.push("страна не найдена в справочнике".into());
    }
    if airport.elevation_ft.is_none() {
        reasons.push("отсутствует измеренная высота".into());
    }
    match airport.timezone.as_deref() {
        Some(value)
            if value.parse::<chrono_tz::Tz>().is_ok()
                && has_resolved_evidence(airport.evidence.get("timezone")) => {}
        Some(_) => {
            reasons.push("timezone невалиден или не имеет разрешённого свидетельства".into())
        }
        None => reasons.push(format!(
            "timezone не определён: {}; candidates={:?}",
            airport.timezone_resolution.outcome, airport.timezone_resolution.candidates
        )),
    }
    if airport.status.is_none() || !has_resolved_evidence(airport.evidence.get("status")) {
        reasons.push("статус open/close не имеет достаточного документированного основания".into());
    }
    if airport.runways.is_empty() {
        reasons.push("в источнике нет классифицированных наземных физических ВПП".into());
    }
    let affecting = airport
        .quarantine
        .iter()
        .filter(|row| row.affects_airport)
        .count();
    if affecting > 0 {
        reasons.push(format!("{affecting} карантинных строк могут представлять обязательные ВПП или серьёзный конфликт"));
    }
    for (field, evidence) in &airport.evidence {
        if evidence
            .iter()
            .any(|item| item.method == EvidenceMethod::Conflicting)
        {
            reasons.push(format!(
                "неразрешённый конфликт обязательного атрибута {field}"
            ));
        }
    }
    for runway in &airport.runways {
        for (field, evidence) in &runway.evidence {
            if evidence
                .iter()
                .any(|item| item.method == EvidenceMethod::Conflicting)
            {
                reasons.push(format!(
                    "неразрешённый конфликт ВПП {} атрибута {field}",
                    runway.id
                ));
            }
        }
    }
    reasons.sort();
    reasons.dedup();
    reasons
}

fn has_resolved_evidence(evidence: Option<&Vec<Evidence>>) -> bool {
    evidence.is_some_and(|items| {
        items.iter().any(|item| {
            matches!(
                item.method,
                EvidenceMethod::SourceReported
                    | EvidenceMethod::Derived
                    | EvidenceMethod::IndependentlyVerified
                    | EvidenceMethod::Recovered
            )
        })
    })
}

pub fn validate_airport(airport: &Airport) -> Vec<String> {
    let draft = AirportDraft {
        icao: airport.icao.clone(),
        latitude: Some(airport.latitude),
        longitude: Some(airport.longitude),
        status: Some(airport.status),
        airport_type: airport.airport_type.clone(),
        country: Some(airport.country.clone()),
        elevation_ft: Some(airport.elevation_ft),
        timezone: Some(airport.timezone.clone()),
        id: Some(airport.id),
        name: airport.name.clone(),
        runways: airport
            .runways
            .iter()
            .map(|r| RunwayDraft {
                id: r.id.clone(),
                length_ft: Some(r.length_ft),
                surface: Some(r.surface.clone()),
                le_ident: r.le_ident.clone(),
                he_ident: r.he_ident.clone(),
                closed: r.closed,
                kind: r.kind,
                provenance: r.provenance.clone(),
                evidence: r.evidence.clone(),
                ..Default::default()
            })
            .collect(),
        provenance: airport.provenance.clone(),
        evidence: airport.evidence.clone(),
        ..Default::default()
    };
    let mut icaos = HashMap::new();
    icaos.insert(draft.icao.clone(), 1);
    let mut ids = HashMap::new();
    ids.insert(airport.id, 1);
    let mut reasons = reasons_for(&draft, &icaos, &ids);
    if airport.runways_count != airport.runways.len() {
        reasons.push("runways_count не совпадает с массивом runways".into());
    }
    if airport
        .runways
        .iter()
        .any(|r| r.kind != RunwayKind::LandRunway)
    {
        reasons.push("строгий экспорт содержит объект, не являющийся наземной ВПП".into());
    }
    reasons
}

pub fn valid_icao(value: &str) -> bool {
    value.len() == 4 && value.bytes().all(|byte| byte.is_ascii_uppercase())
}

fn to_airport(mut draft: AirportDraft) -> Airport {
    let runway_count = draft.runways.len();
    draft.evidence.entry("runways_count".into()).or_default().push(Evidence::derived(
        "Aeroporty validation", draft.id.map(|v| v.to_string()).unwrap_or_default(),
        format!("{} classified land-runway rows", runway_count), runway_count.to_string(),
        "число только полностью валидных наземных физических ВПП; направления одной строки не удваиваются",
    ));
    let runways = draft
        .runways
        .into_iter()
        .map(|runway| Runway {
            id: runway.id,
            length_ft: runway.length_ft.expect("validated land runway length"),
            surface: runway.surface.expect("validated land runway surface"),
            le_ident: runway.le_ident,
            he_ident: runway.he_ident,
            closed: runway.closed,
            kind: runway.kind,
            provenance: runway.provenance,
            evidence: runway.evidence,
        })
        .collect();
    Airport {
        icao: draft.icao,
        latitude: draft.latitude.expect("validated latitude"),
        longitude: draft.longitude.expect("validated longitude"),
        status: draft.status.expect("validated status"),
        airport_type: draft.airport_type,
        country: draft.country.expect("validated country"),
        elevation_ft: draft.elevation_ft.expect("validated elevation"),
        timezone: draft.timezone.expect("validated timezone"),
        runways_count: runway_count,
        id: draft.id.expect("validated id"),
        name: draft.name,
        runways,
        provenance: draft.provenance,
        evidence: draft.evidence,
    }
}
