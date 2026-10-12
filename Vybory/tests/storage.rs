use std::path::Path;

use elections::{
    CAMPAIGN_ID,
    db::Database,
    export::export_all,
    model::{ApiReport, CommissionNode, Election, HierarchyContext, PrecinctJob},
    validation::{parse_report, validate_protocol},
};

fn election() -> Election {
    Election {
        id: CAMPAIGN_ID,
        vrn: Some("fixture-vrn".to_owned()),
        external_id: "fixture-election".to_owned(),
        name: elections::ELECTION_NAME.to_owned(),
        voting_date: elections::ELECTION_DATE.to_owned(),
        start_voting_day: None,
        finish_voting_day: None,
        is_multi_day_voting: Some(true),
        election_level: None,
        kind: None,
        system_type: None,
    }
}

fn node(id: &str, number: i64) -> CommissionNode {
    CommissionNode {
        id: Some(number),
        external_id: id.to_owned(),
        name: format!("УИК №{number}"),
        commission_type: 5,
        number: Some(number),
        parent_id: None,
        has_children: false,
        children: vec![],
        protocol1_signed: None,
        protocol2_signed: Some(true),
    }
}

fn context(region: &str, tik: &str) -> HierarchyContext {
    HierarchyContext {
        region_id: Some(region.to_owned()),
        region_name: Some(format!("Регион {region}")),
        federal_district: Some("Тестовый округ".to_owned()),
        iksrf_id: Some(format!("iksrf-{region}")),
        iksrf_name: Some(format!("ИКСРФ {region}")),
        oik_id: None,
        oik_name: None,
        tik_id: Some(tik.to_owned()),
        tik_name: Some(format!("ТИК {tik}")),
    }
}

fn fixture_report() -> ApiReport {
    serde_json::from_str(include_str!("fixtures/uik_1.json")).unwrap()
}

fn database(path: &Path) -> Database {
    let mut db = Database::open(path).unwrap();
    db.save_election(&election()).unwrap();
    db
}

#[test]
fn same_precinct_number_in_different_regions_is_not_a_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = database(&dir.path().join("test.sqlite"));
    for (id, region, tik) in [("uik-a", "01", "tik-a"), ("uik-b", "02", "tik-b")] {
        let n = node(id, 1);
        let ctx = context(region, tik);
        db.upsert_commission(CAMPAIGN_ID, &n, None, &ctx, "fixture")
            .unwrap();
        db.upsert_precinct_and_job(CAMPAIGN_ID, &n, &ctx, "fixture")
            .unwrap();
    }
    assert_eq!(db.count("precincts").unwrap(), 2);
}

#[test]
fn electronic_voting_precinct_is_classified_separately() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = database(&dir.path().join("test.sqlite"));
    let mut n = node("deg-uik", 9000);
    n.name = "УИК дистанционного электронного голосования".to_owned();
    let ctx = context("77", "tik-deg");
    db.upsert_commission(CAMPAIGN_ID, &n, None, &ctx, "fixture")
        .unwrap();
    db.upsert_precinct_and_job(CAMPAIGN_ID, &n, &ctx, "fixture")
        .unwrap();
    let category = db
        .precinct_category(CAMPAIGN_ID, "deg-uik")
        .unwrap()
        .unwrap();
    assert_eq!(category, "deg");
}

#[test]
fn foreign_territory_is_classified_separately() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = database(&dir.path().join("test.sqlite"));
    let n = node("foreign-uik", 9901);
    let mut ctx = context("99", "tik-foreign");
    ctx.region_name = Some("Зарубежная территория".to_owned());
    ctx.tik_name = Some("Территория за пределами РФ".to_owned());
    ctx.federal_district = Some("Специальные и зарубежные".to_owned());
    db.upsert_commission(CAMPAIGN_ID, &n, None, &ctx, "fixture")
        .unwrap();
    db.upsert_precinct_and_job(CAMPAIGN_ID, &n, &ctx, "fixture")
        .unwrap();
    let category = db
        .precinct_category(CAMPAIGN_ID, "foreign-uik")
        .unwrap()
        .unwrap();
    assert_eq!(category, "foreign");
}

#[test]
fn baikonur_is_classified_separately() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = database(&dir.path().join("test.sqlite"));
    let n = node("baikonur-uik", 9902);
    let mut ctx = context("99", "tik-baikonur");
    ctx.tik_name = Some("Город Байконур (Республика Казахстан)".to_owned());
    ctx.federal_district = None;
    db.upsert_commission(CAMPAIGN_ID, &n, None, &ctx, "fixture")
        .unwrap();
    db.upsert_precinct_and_job(CAMPAIGN_ID, &n, &ctx, "fixture")
        .unwrap();
    assert_eq!(
        db.precinct_category(CAMPAIGN_ID, "baikonur-uik")
            .unwrap()
            .as_deref(),
        Some("baikonur")
    );
}

#[test]
fn report_for_another_uik_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = database(&dir.path().join("test.sqlite"));
    let n = node("requested-uik", 1);
    let ctx = context("01", "tik-a");
    db.upsert_commission(CAMPAIGN_ID, &n, None, &ctx, "fixture")
        .unwrap();
    db.upsert_precinct_and_job(CAMPAIGN_ID, &n, &ctx, "fixture")
        .unwrap();
    let job = PrecinctJob {
        election_id: CAMPAIGN_ID,
        uik_id: n.external_id,
        uik_number: Some(1),
        region_name: ctx.region_name,
        tik_id: ctx.tik_id,
    };
    let report = fixture_report();
    let raw = serde_json::to_string(&report).unwrap();
    let (parsed, issues) = parse_report(&report);
    let outcome = validate_protocol(&parsed, issues);
    assert!(
        db.save_protocol(&job, "fixture-report", &raw, &parsed, &outcome)
            .is_err()
    );
    assert_eq!(db.count("protocols").unwrap(), 0);
}

#[test]
fn missing_signature_date_is_not_replaced_by_publication_date() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = database(&dir.path().join("test.sqlite"));
    let n = node("fixture-uik-1", 1);
    let ctx = context("01", "tik-a");
    db.upsert_commission(CAMPAIGN_ID, &n, None, &ctx, "fixture")
        .unwrap();
    db.upsert_precinct_and_job(CAMPAIGN_ID, &n, &ctx, "fixture")
        .unwrap();
    let job = PrecinctJob {
        election_id: CAMPAIGN_ID,
        uik_id: n.external_id,
        uik_number: Some(1),
        region_name: ctx.region_name,
        tik_id: ctx.tik_id,
    };
    let mut report = fixture_report();
    report.body.sign_date_time = None;
    let raw = serde_json::to_string(&report).unwrap();
    let (parsed, issues) = parse_report(&report);
    let outcome = validate_protocol(&parsed, issues);
    db.save_protocol(&job, "fixture-report", &raw, &parsed, &outcome)
        .unwrap();
    let output = dir.path().join("output");
    export_all(&db, &output).unwrap();
    let mut reader = csv::Reader::from_path(output.join("all_uiks.csv")).unwrap();
    let headers = reader.headers().unwrap().clone();
    let row = reader.records().next().unwrap().unwrap();
    let protocol_date = headers
        .iter()
        .position(|name| name == "protocol_date")
        .unwrap();
    let source_created_at = headers
        .iter()
        .position(|name| name == "source_created_at")
        .unwrap();
    assert_eq!(&row[protocol_date], "");
    assert_eq!(&row[source_created_at], "2026-09-21T10:00:00+03:00");
}

#[test]
fn interrupted_job_is_resumed() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = database(&dir.path().join("test.sqlite"));
    let n = node("uik-a", 1);
    let ctx = context("01", "tik-a");
    db.upsert_commission(CAMPAIGN_ID, &n, None, &ctx, "fixture")
        .unwrap();
    db.upsert_precinct_and_job(CAMPAIGN_ID, &n, &ctx, "fixture")
        .unwrap();
    let job = db
        .pending_jobs(CAMPAIGN_ID, None, None, None, None, None, false)
        .unwrap()
        .remove(0);
    db.mark_running(&job).unwrap();
    assert_eq!(db.reset_interrupted_jobs(CAMPAIGN_ID).unwrap(), 1);
    assert_eq!(
        db.pending_jobs(CAMPAIGN_ID, None, None, None, None, None, false)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn repeated_save_is_idempotent_and_export_has_one_row() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = database(&dir.path().join("test.sqlite"));
    let n = node("fixture-uik-1", 1);
    let ctx = context("01", "tik-a");
    db.upsert_commission(CAMPAIGN_ID, &n, None, &ctx, "fixture")
        .unwrap();
    db.upsert_precinct_and_job(CAMPAIGN_ID, &n, &ctx, "fixture")
        .unwrap();
    let job = PrecinctJob {
        election_id: CAMPAIGN_ID,
        uik_id: n.external_id.clone(),
        uik_number: Some(1),
        region_name: ctx.region_name.clone(),
        tik_id: ctx.tik_id.clone(),
    };
    let report = fixture_report();
    let raw = serde_json::to_string(&report).unwrap();
    let (parsed, issues) = parse_report(&report);
    let outcome = validate_protocol(&parsed, issues);
    db.save_protocol(&job, "fixture-report", &raw, &parsed, &outcome)
        .unwrap();
    db.save_protocol(&job, "fixture-report", &raw, &parsed, &outcome)
        .unwrap();
    assert_eq!(db.count("protocols").unwrap(), 1);
    assert_eq!(db.count("protocol_versions").unwrap(), 1);

    let output = dir.path().join("output");
    export_all(&db, &output).unwrap();
    let csv = std::fs::read_to_string(output.join("all_uiks.csv")).unwrap();
    assert_eq!(csv.lines().count(), 2);
    assert!(csv.lines().next().unwrap().contains("line_22"));
    let header = csv.lines().next().unwrap();
    assert!(header.contains("source_created_at"));
    assert!(
        csv.lines()
            .nth(1)
            .unwrap()
            .contains("2026-09-21T10:00:00+03:00")
    );
}

#[test]
fn changed_official_report_is_saved_as_a_new_version() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = database(&dir.path().join("test.sqlite"));
    let n = node("fixture-uik-1", 1);
    let ctx = context("01", "tik-a");
    db.upsert_commission(CAMPAIGN_ID, &n, None, &ctx, "fixture")
        .unwrap();
    db.upsert_precinct_and_job(CAMPAIGN_ID, &n, &ctx, "fixture")
        .unwrap();
    let job = PrecinctJob {
        election_id: CAMPAIGN_ID,
        uik_id: n.external_id.clone(),
        uik_number: Some(1),
        region_name: ctx.region_name.clone(),
        tik_id: ctx.tik_id.clone(),
    };
    for report_id in ["version-one", "version-two"] {
        let mut report = fixture_report();
        report.id = report_id.to_owned();
        let raw = serde_json::to_string(&report).unwrap();
        let (parsed, issues) = parse_report(&report);
        let outcome = validate_protocol(&parsed, issues);
        db.save_protocol(&job, "fixture-report", &raw, &parsed, &outcome)
            .unwrap();
    }
    assert_eq!(db.count("protocols").unwrap(), 1);
    assert_eq!(db.count("protocol_versions").unwrap(), 2);
}

#[test]
fn failed_job_keeps_previously_saved_data() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = database(&dir.path().join("test.sqlite"));
    let n = node("uik-a", 1);
    let ctx = context("01", "tik-a");
    db.upsert_commission(CAMPAIGN_ID, &n, None, &ctx, "fixture")
        .unwrap();
    db.upsert_precinct_and_job(CAMPAIGN_ID, &n, &ctx, "fixture")
        .unwrap();
    let job = PrecinctJob {
        election_id: CAMPAIGN_ID,
        uik_id: "uik-a".to_owned(),
        uik_number: Some(1),
        region_name: None,
        tik_id: None,
    };
    db.record_job_error(&job, "download", "сетевая ошибка")
        .unwrap();
    assert_eq!(db.count("precincts").unwrap(), 1);
    assert_eq!(db.count("download_errors").unwrap(), 1);
    assert_eq!(
        db.pending_jobs(CAMPAIGN_ID, None, None, None, None, None, true)
            .unwrap()
            .len(),
        1
    );
}
