use aeroporty::{
    config::Config,
    download,
    enrichment::{enrich_from_local, meters_to_feet},
    export::{ExportFormat, export_dataset, verify_exports},
    models::{
        AirportDraft, AirportStatus, Dataset, Evidence, EvidenceMap, EvidenceMethod,
        ProcessingStats, QuarantineCategory, RunwayDraft, RunwayKind, TimezoneResolution,
    },
    parser::parse_ourairports,
    report::{ReportMetadata, build_report},
    runway_classification::classify_runway,
    validation::{valid_icao, validate_all},
};

fn source(field: &str, value: &str) -> Evidence {
    Evidence::source_reported("fixture", "1", value, value, format!("valid {field}"))
}

fn valid_draft(id: u64, icao: &str) -> AirportDraft {
    let mut evidence = EvidenceMap::new();
    for (field, value) in [
        ("icao", icao),
        ("latitude", "55.9726"),
        ("longitude", "37.4146"),
        ("type", "large_airport"),
        ("country", "Testland"),
        ("elevation_ft", "622"),
        ("id", "1"),
        ("name", "Airport"),
    ] {
        evidence
            .entry(field.into())
            .or_default()
            .push(source(field, value));
    }
    evidence
        .entry("status".into())
        .or_default()
        .push(Evidence::derived(
            "fixture",
            "1",
            "large_airport",
            "open",
            "documented fixture policy",
        ));
    evidence
        .entry("timezone".into())
        .or_default()
        .push(Evidence::derived(
            "fixture",
            "1",
            "coordinates",
            "Europe/Moscow",
            "unique polygon",
        ));
    let mut runway_evidence = EvidenceMap::new();
    for (field, value) in [("id", "R1"), ("length_ft", "10500"), ("surface", "ASPH")] {
        runway_evidence
            .entry(field.into())
            .or_default()
            .push(source(field, value));
    }
    AirportDraft {
        icao: icao.into(),
        latitude: Some(55.9726),
        longitude: Some(37.4146),
        status: Some(AirportStatus::Open),
        airport_type: "large_airport".into(),
        country: Some("Testland".into()),
        elevation_ft: Some(622),
        timezone: Some("Europe/Moscow".into()),
        timezone_resolution: TimezoneResolution {
            candidates: vec!["Europe/Moscow".into()],
            dataset_version: "fixture".into(),
            outcome: "unique_polygon_match".into(),
        },
        id: Some(id),
        name: format!("Airport {icao}"),
        runways: vec![RunwayDraft {
            id: "R1".into(),
            length_ft: Some(10_500.0),
            width_ft: Some(150.0),
            surface: Some("ASPH".into()),
            le_ident: Some("06".into()),
            he_ident: Some("24".into()),
            closed: false,
            kind: RunwayKind::LandRunway,
            classification_reason: "fixture".into(),
            evidence: runway_evidence,
            ..Default::default()
        }],
        evidence,
        ..Default::default()
    }
}

fn classified(low: &str, high: &str, surface: &str, length: f64) -> RunwayDraft {
    RunwayDraft {
        id: "R".into(),
        length_ft: Some(length),
        width_ft: Some(50.0),
        surface: Some(surface.into()),
        le_ident: Some(low.into()),
        he_ident: Some(high.into()),
        ..Default::default()
    }
}

#[test]
fn validates_icao_format() {
    assert!(valid_icao("UUEE"));
    assert!(!valid_icao("UU1E"));
    assert!(!valid_icao("uuee"));
    assert!(!valid_icao("ABCDE"));
}

#[test]
fn classifies_waterway_helipad_and_short_land_runway() {
    assert_eq!(
        classify_runway("small_airport", &classified("09W", "27W", "WATER", 5000.0)).0,
        RunwayKind::Waterway
    );
    assert_eq!(
        classify_runway(
            "large_airport",
            &classified("H-NORTH", "H-SOUTH", "CONC", 100.0)
        )
        .0,
        RunwayKind::Helipad
    );
    assert_eq!(
        classify_runway("small_airport", &classified("09", "27", "GRASS", 450.0)).0,
        RunwayKind::LandRunway
    );
}

#[test]
fn one_row_with_09_27_is_one_runway() {
    let outcome = validate_all(vec![valid_draft(1, "TAAA")]);
    assert_eq!(outcome.airports.len(), 1);
    assert_eq!(outcome.airports[0].runways_count, 1);
}

#[test]
fn rejects_missing_required_values_and_keeps_incomplete_runway_in_quarantine() {
    let mut draft = valid_draft(1, "TAAA");
    draft.elevation_ft = None;
    draft.runways[0].surface = None;
    let outcome = validate_all(vec![draft]);
    assert!(outcome.airports.is_empty());
    assert_eq!(outcome.rejected.len(), 1);
    assert!(outcome.quarantine.iter().any(|row| row.category
        == QuarantineCategory::IncompleteLandRunway
        && row.affects_airport));
}

#[test]
fn unresolved_quarantine_never_enters_strict_dataset() {
    let mut draft = valid_draft(1, "TAAA");
    draft.quarantine.push(aeroporty::models::QuarantineRecord {
        source: "fixture".into(),
        row_number: Some(2),
        airport_id: Some(1),
        icao: Some("TAAA".into()),
        record_id: Some("X".into()),
        category: QuarantineCategory::UnknownType,
        reason: "unknown".into(),
        affects_airport: true,
        raw: None,
    });
    let outcome = validate_all(vec![draft]);
    assert!(outcome.airports.is_empty());
    assert_eq!(outcome.quarantine.len(), 1);
}

#[test]
fn status_without_evidence_is_not_assumed_open() {
    let mut draft = valid_draft(1, "TAAA");
    draft.evidence.remove("status");
    assert!(
        validate_all(vec![draft]).rejected[0]
            .reasons
            .iter()
            .any(|reason| reason.contains("статус"))
    );
}

#[test]
fn ambiguous_timezone_is_rejected_without_default() {
    let mut draft = valid_draft(1, "TAAA");
    draft.timezone = None;
    draft.timezone_resolution = TimezoneResolution {
        candidates: vec!["Asia/Shanghai".into(), "Asia/Urumqi".into()],
        dataset_version: "fixture".into(),
        outcome: "ambiguous_polygon_match".into(),
    };
    assert!(
        validate_all(vec![draft]).rejected[0]
            .reasons
            .iter()
            .any(|reason| reason.contains("candidates"))
    );
}

#[test]
fn same_source_is_not_independent_verification() {
    let outcome = validate_all(vec![valid_draft(1, "TAAA")]);
    let report = build_report(
        &outcome.airports,
        &outcome.rejected,
        ProcessingStats::default(),
        ReportMetadata {
            sources: vec![],
            conflicts: vec![],
            recovered_records: 0,
            recovered_attributes: 0,
            timezone_dataset_version: "fixture".into(),
            quarantine: outcome.quarantine,
            geographic_issues: outcome.geographic_issues,
            faa: None,
            phase3: None,
            anac: None,
            phase4: None,
            phase5: None,
        },
    );
    assert_eq!(report.statistics.attributes.filled, 14);
    assert_eq!(report.statistics.attributes.independently_verified, 0);
    assert!(report.statistics.attributes.source_reported > 0);
    assert!(report.statistics.attributes.derived > 0);
}

#[test]
fn parser_quarantines_bad_reference_without_losing_valid_airport_runway() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_source_fixture(
        dir.path(),
        "10,1,TAAA,10000,150,ASPH,1,0,06,,,,,,24,,,,,\n11,BAD,TAAA,9000,100,ASPH,0,0,09,,,,,,27,,,,,\n",
    );
    let (drafts, stats, quarantine) = parse_ourairports(dir.path()).expect("parse");
    assert_eq!(stats.source_airports, 2);
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].runways.len(), 1);
    assert!(
        quarantine
            .iter()
            .any(|row| row.category == QuarantineCategory::InvalidReference)
    );
}

#[test]
fn ksea_style_bogus_closed_row_does_not_destroy_valid_runways() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_source_fixture(
        dir.path(),
        "10,1,TAAA,10000,150,ASPH,1,0,16L,,,,,,34R,,,,,\n302282,1,TAAA,,, ,0,1,lower,,,,,,high,,,,,\n",
    );
    let (drafts, _, _) = parse_ourairports(dir.path()).expect("parse");
    assert_eq!(drafts[0].runways.len(), 1);
    assert!(
        drafts[0]
            .quarantine
            .iter()
            .any(|row| row.category == QuarantineCategory::InvalidRecord && !row.affects_airport)
    );
}

#[test]
fn local_enrichment_recovers_missing_values_and_preserves_conflicts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("enrichment.json");
    std::fs::write(&path, r#"{"airports":[{"icao":"TAAA","source":"official fixture","source_record_id":"A1","effective_at":"2026-01-01","elevation_ft":777,"timezone":"Europe/London","runways":[{"id":"R1","length_m":1000.0,"surface":"GRASS"}]}]}"#).expect("fixture");
    let mut draft = valid_draft(1, "TAAA");
    draft.elevation_ft = None;
    draft.runways[0].length_ft = None;
    let outcome = enrich_from_local(std::slice::from_mut(&mut draft), &path).expect("enrich");
    assert_eq!(draft.elevation_ft, Some(777));
    assert!(draft.runways[0].length_ft.is_some());
    assert_eq!(outcome.recovered_attributes, 2);
    assert_eq!(outcome.conflicts.len(), 2);
    assert!(
        draft.evidence["timezone"]
            .iter()
            .any(|item| item.method == EvidenceMethod::Conflicting)
    );
}

#[test]
fn vccc_style_conflict_blocks_strict_export() {
    let mut draft = valid_draft(1, "VCCC");
    draft.runways[0]
        .evidence
        .entry("length_ft".into())
        .or_default()
        .push(Evidence {
            source: "official AIP fixture".into(),
            source_record_id: Some("AD-2.VCCC".into()),
            retrieved_at: None,
            effective_at: Some("2025-04-17".into()),
            raw_value: "1400 m".into(),
            normalized_value: "4593.175853 ft".into(),
            method: EvidenceMethod::Conflicting,
            validation: "publication conflict retained".into(),
            conflict: Some("OurAirports=6013 ft".into()),
        });
    let outcome = validate_all(vec![draft]);
    assert!(outcome.airports.is_empty());
    assert!(
        outcome.rejected[0]
            .reasons
            .iter()
            .any(|reason| reason.contains("конфликт ВПП"))
    );
}

#[test]
fn all_exports_round_trip_with_evidence() {
    let outcome = validate_all(vec![valid_draft(1, "TAAA")]);
    let dataset = Dataset {
        airports: outcome.airports,
    };
    let dir = tempfile::tempdir().expect("tempdir");
    export_dataset(&dataset, dir.path(), &ExportFormat::ALL).expect("export");
    assert_eq!(verify_exports(dir.path()).expect("verify"), dataset);
}

#[test]
fn converts_meters_with_required_factor() {
    assert!((meters_to_feet(1000.0) - 3280.839895).abs() < 1e-9);
}

#[tokio::test]
async fn offline_mode_rejects_wrong_schema() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = dir.path().join("cache");
    std::fs::create_dir_all(&cache).expect("cache");
    for name in ["airports.csv", "runways.csv", "countries.csv"] {
        std::fs::write(cache.join(name), "one,two\nvalue,value\n").expect("csv");
    }
    let mut config = Config::from_root(dir.path());
    config.offline = true;
    assert!(download::update_sources(&config).await.is_err());
}

#[tokio::test]
async fn offline_mode_accepts_schema_valid_cache() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = dir.path().join("cache");
    std::fs::create_dir_all(&cache).expect("cache");
    write_source_fixture(&cache, "10,1,TAAA,10000,150,ASPH,1,0,06,,,,,,24,,,,,\n");
    let mut config = Config::from_root(dir.path());
    config.offline = true;
    assert_eq!(
        download::update_sources(&config)
            .await
            .expect("offline")
            .len(),
        3
    );
}

fn write_source_fixture(dir: &std::path::Path, runway_rows: &str) {
    std::fs::write(
        dir.join("countries.csv"),
        "id,code,name,continent,wikipedia_link,keywords\n1,TL,Testland,EU,,\n",
    )
    .expect("countries");
    std::fs::write(dir.join("airports.csv"), "id,ident,type,name,latitude_deg,longitude_deg,elevation_ft,continent,iso_country,iso_region,municipality,scheduled_service,gps_code,iata_code,local_code,home_link,wikipedia_link,keywords,score,last_updated,icao_code\n1,TAAA,large_airport,Alpha,55,37,600,EU,TL,,,,,,,,,,,,TAAA\n2,NOPE,small_airport,No ICAO,1,2,3,EU,TL,,,,,,,,,,,,\n").expect("airports");
    let header = "id,airport_ref,airport_ident,length_ft,width_ft,surface,lighted,closed,le_ident,le_latitude_deg,le_longitude_deg,le_elevation_ft,le_heading_degT,le_displaced_threshold_ft,he_ident,he_latitude_deg,he_longitude_deg,he_elevation_ft,he_heading_degT,he_displaced_threshold_ft\n";
    std::fs::write(dir.join("runways.csv"), format!("{header}{runway_rows}")).expect("runways");
}
