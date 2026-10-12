use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use anyhow::{Context, Result};
use futures_util::{StreamExt, stream};
use serde::Serialize;
use tokio::sync::Mutex;

use crate::{
    CAMPAIGN_ID,
    api::OfficialApi,
    db::{Database, now},
    districts::federal_district,
    model::{CommissionNode, HierarchyContext, ProtocolSchema, ProtocolSchemaLine},
    validation::{parse_report, validate_protocol},
};

#[derive(Debug, Clone, Default)]
pub struct Filters {
    pub federal_district: Option<String>,
    pub region: Option<String>,
    pub tik: Option<String>,
    pub uik: Option<i64>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DiscoverSummary {
    pub commissions: usize,
    pub regions: usize,
    pub oiks: usize,
    pub tiks: usize,
    pub uiks: usize,
    pub unique_uiks_in_database: i64,
}

#[derive(Debug, Clone)]
struct QueueItem {
    node: CommissionNode,
    parent_id: Option<String>,
    context: HierarchyContext,
}

pub async fn discover(
    api: &OfficialApi,
    db: &mut Database,
    filters: &Filters,
    output_root: &Path,
) -> Result<DiscoverSummary> {
    let election = api.election(CAMPAIGN_ID).await?;
    anyhow::ensure!(
        election.voting_date == crate::ELECTION_DATE,
        "официальный API вернул другую дату кампании: {}",
        election.voting_date
    );
    anyhow::ensure!(
        election.name == crate::ELECTION_NAME,
        "официальный API вернул неожиданное название кампании"
    );
    db.save_election(&election)?;

    let root = api.commission_tree(CAMPAIGN_ID, None).await?;
    let root_source = commission_source_url(CAMPAIGN_ID, None);
    db.upsert_commission(
        CAMPAIGN_ID,
        &root,
        None,
        &HierarchyContext::default(),
        &root_source,
    )?;
    let root_id = root.external_id.clone();
    let mut queue = VecDeque::new();
    for child in root.children {
        queue.push_back(QueueItem {
            node: child,
            parent_id: Some(root_id.clone()),
            context: HierarchyContext::default(),
        });
    }

    let mut summary = DiscoverSummary {
        commissions: 1,
        ..DiscoverSummary::default()
    };
    while let Some(mut item) = queue.pop_front() {
        update_context(&mut item.context, &item.node);
        if item.node.commission_type == 2 && !matches_region(&item.context, filters) {
            continue;
        }
        if let Some(fd) = filters.federal_district.as_deref()
            && item.context.federal_district.as_deref() != Some(fd)
        {
            continue;
        }
        let source = commission_source_url(CAMPAIGN_ID, Some(&item.node.external_id));
        db.upsert_commission(
            CAMPAIGN_ID,
            &item.node,
            item.parent_id.as_deref(),
            &item.context,
            &source,
        )?;
        summary.commissions += 1;
        match item.node.commission_type {
            2 => summary.regions += 1,
            3 => summary.oiks += 1,
            4 => summary.tiks += 1,
            5 => summary.uiks += 1,
            _ => {}
        }
        if item.node.commission_type == 4
            && let Some(tik) = filters.tik.as_deref()
            && item.node.external_id != tik
            && item.node.name != tik
        {
            continue;
        }
        if item.node.commission_type == 5 {
            if filters.uik.is_none() || item.node.number == filters.uik {
                db.upsert_precinct_and_job(CAMPAIGN_ID, &item.node, &item.context, &source)?;
            }
            continue;
        }
        if item.node.has_children {
            let full = api
                .commission_tree(CAMPAIGN_ID, Some(&item.node.external_id))
                .await
                .with_context(|| format!("не удалось раскрыть комиссию {}", item.node.name))?;
            for child in full.children {
                queue.push_back(QueueItem {
                    node: child,
                    parent_id: Some(item.node.external_id.clone()),
                    context: item.context.clone(),
                });
            }
        }
        if summary.commissions.is_multiple_of(250) {
            tracing::info!(
                commissions = summary.commissions,
                uiks = summary.uiks,
                "обнаружение комиссий"
            );
        }
    }

    confirm_protocol_schema(api, db, &election.external_id, output_root).await?;
    summary.unique_uiks_in_database = db.count("precincts")?;
    Ok(summary)
}

fn matches_region(context: &HierarchyContext, filters: &Filters) -> bool {
    let Some(expected) = filters.region.as_deref() else {
        return true;
    };
    context.region_id.as_deref() == Some(expected)
        || context.region_name.as_deref() == Some(expected)
}

fn update_context(context: &mut HierarchyContext, node: &CommissionNode) {
    match node.commission_type {
        2 => {
            context.region_id = node.number.map(|number| format!("{number:02}"));
            context.region_name = Some(node.name.clone());
            context.federal_district = Some(federal_district(&node.name).to_owned());
            context.iksrf_id = Some(node.external_id.clone());
            context.iksrf_name = Some(node.name.clone());
        }
        3 => {
            context.oik_id = Some(node.external_id.clone());
            context.oik_name = Some(node.name.clone());
        }
        4 => {
            context.tik_id = Some(node.external_id.clone());
            context.tik_name = Some(node.name.clone());
        }
        _ => {}
    }
}

fn commission_source_url(election_id: i64, classifier_id: Option<&str>) -> String {
    match classifier_id {
        Some(id) => format!("http://www.izbirkom.ru/election/{election_id}/commission/{id}/"),
        None => format!("http://www.izbirkom.ru/election/{election_id}"),
    }
}

async fn confirm_protocol_schema(
    api: &OfficialApi,
    db: &mut Database,
    election_external_id: &str,
    output_root: &Path,
) -> Result<()> {
    let root = api.commission_tree(CAMPAIGN_ID, None).await?;
    let (report, _) = api.protocol(&root.external_id).await?;
    let (parsed, parse_issues) = parse_report(&report);
    let validation = validate_protocol(&parsed, parse_issues);
    anyhow::ensure!(
        validation.strict_valid,
        "официальная схема ЦИК не прошла проверку: {:?}",
        validation.issues
    );
    let source_url = api.report_url(&root.external_id);
    let lines = parsed
        .lines
        .values()
        .map(|line| ProtocolSchemaLine {
            line_number: line.line_number,
            official_text: line.official_text.clone(),
            kind: if line.line_number <= 12 {
                "summary".to_owned()
            } else {
                "party".to_owned()
            },
            party_name: line.party_name.clone(),
        })
        .collect::<Vec<_>>();
    let schema = ProtocolSchema {
        election_id: CAMPAIGN_ID,
        election_external_id: election_external_id.to_owned(),
        protocol_number: 2,
        report_number: 242,
        source_url: source_url.clone(),
        confirmed_at: now(),
        lines,
    };
    std::fs::create_dir_all(output_root)?;
    std::fs::write(
        output_root.join("protocol_schema.json"),
        serde_json::to_vec_pretty(&schema)?,
    )?;
    let parties = schema
        .lines
        .iter()
        .filter(|line| line.line_number >= 13)
        .collect::<Vec<_>>();
    std::fs::write(
        output_root.join("political_parties.json"),
        serde_json::to_vec_pretty(&parties)?,
    )?;
    for line in parties {
        db.conn.execute(
            "INSERT INTO political_parties(election_id, protocol_line, ballot_position,
               official_name, source_url) VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(election_id, protocol_line) DO UPDATE SET
               ballot_position=excluded.ballot_position, official_name=excluded.official_name,
               source_url=excluded.source_url",
            rusqlite::params![
                CAMPAIGN_ID,
                line.line_number,
                i64::from(line.line_number - 12),
                line.party_name,
                source_url
            ],
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RunSummary {
    pub selected: usize,
    pub completed: usize,
    pub failed: usize,
    pub interrupted: bool,
    pub http_requests: u64,
    pub http_retries: u64,
    pub http_errors: u64,
    pub network_errors: u64,
    pub duration_seconds: f64,
}

pub async fn run_downloads(
    api: OfficialApi,
    db: Arc<Mutex<Database>>,
    filters: Filters,
    concurrency: usize,
    include_failed: bool,
    cache_root: PathBuf,
) -> Result<RunSummary> {
    let started = Instant::now();
    anyhow::ensure!(
        concurrency > 0 && concurrency <= 64,
        "--concurrency должен быть от 1 до 64"
    );
    let jobs = {
        let mut db = db.lock().await;
        db.reset_interrupted_jobs(CAMPAIGN_ID)?;
        db.pending_jobs(
            CAMPAIGN_ID,
            filters.federal_district.as_deref(),
            filters.region.as_deref(),
            filters.tik.as_deref(),
            filters.uik,
            filters.limit,
            include_failed,
        )?
    };
    let selected = jobs.len();
    let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let failed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let interrupted = Arc::new(AtomicBool::new(false));
    let signal_flag = interrupted.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_flag.store(true, Ordering::SeqCst);
            tracing::warn!("получен Ctrl+C; новые задания будут пропущены");
        }
    });
    std::fs::create_dir_all(&cache_root)?;
    stream::iter(jobs)
        .for_each_concurrent(concurrency, |job| {
            let api = api.clone();
            let db = db.clone();
            let cache_root = cache_root.clone();
            let completed = completed.clone();
            let failed = failed.clone();
            let interrupted = interrupted.clone();
            async move {
                if interrupted.load(Ordering::SeqCst) {
                    return;
                }
                {
                    let mut db = db.lock().await;
                    if let Err(error) = db.mark_running(&job) {
                        tracing::error!(uik=%job.uik_id, %error, "не удалось отметить задание");
                        failed.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                }
                match api.protocol(&job.uik_id).await {
                    Ok((report, raw)) => {
                        let cache_path = cache_root.join(format!("{}.json", job.uik_id));
                        if let Err(error) = tokio::fs::write(&cache_path, raw.as_bytes()).await {
                            tracing::warn!(path=%cache_path.display(), %error, "не удалось записать кэш");
                        }
                        let (parsed, issues) = parse_report(&report);
                        let validation = validate_protocol(&parsed, issues);
                        let source_url = api.report_url(&job.uik_id);
                        let result = db.lock().await.save_protocol(
                            &job,
                            &source_url,
                            &raw,
                            &parsed,
                            &validation,
                        );
                        match result {
                            Ok(()) => {
                                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                                if done.is_multiple_of(100) {
                                    tracing::info!(done, total=selected, "загрузка протоколов");
                                }
                            }
                            Err(error) => {
                                tracing::error!(uik=%job.uik_id, %error, "ошибка сохранения");
                                failed.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                    Err(error) => {
                        let message = format!("{error:#}");
                        tracing::warn!(uik=%job.uik_id, %error, "протокол не получен");
                        if let Err(db_error) = db
                            .lock()
                            .await
                            .record_job_error(&job, "download", &message)
                        {
                            tracing::error!(%db_error, "не удалось сохранить ошибку");
                        }
                        failed.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        })
        .await;
    Ok(RunSummary {
        selected,
        completed: completed.load(Ordering::Relaxed),
        failed: failed.load(Ordering::Relaxed),
        interrupted: interrupted.load(Ordering::SeqCst),
        http_requests: api.metrics.requests.load(Ordering::Relaxed),
        http_retries: api.metrics.retries.load(Ordering::Relaxed),
        http_errors: api.metrics.errors.load(Ordering::Relaxed),
        network_errors: api.metrics.network_errors.load(Ordering::Relaxed),
        duration_seconds: started.elapsed().as_secs_f64(),
    })
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct VerifySummary {
    pub complete_tiks_checked: usize,
    pub incomplete_tiks_skipped: usize,
    pub differing_lines: usize,
}

fn aggregate_difference(uik_sum: Option<i64>, official: Option<i64>) -> Option<i64> {
    match (uik_sum, official) {
        (Some(left), Some(right)) => Some(left - right),
        _ => None,
    }
}

fn is_complete_tik(total: i64, completed: i64) -> bool {
    total == completed
}

pub async fn verify_aggregates(
    api: &OfficialApi,
    db: &mut Database,
    filters: &Filters,
) -> Result<VerifySummary> {
    let tiks = {
        let mut stmt = db.conn.prepare(
            "SELECT p.tik_id, COUNT(*) total,
               SUM(CASE WHEN j.status='completed' THEN 1 ELSE 0 END) completed
             FROM precincts p JOIN download_jobs j
               ON j.election_id=p.election_id AND j.uik_id=p.uik_id
             WHERE p.election_id=?1
               AND (?2 IS NULL OR p.region_name=?2 OR p.region_id=?2)
               AND (?3 IS NULL OR p.tik_id=?3 OR p.tik_name=?3)
             GROUP BY p.tik_id ORDER BY p.region_id, p.tik_id",
        )?;
        stmt.query_map(
            rusqlite::params![CAMPAIGN_ID, filters.region, filters.tik],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut summary = VerifySummary::default();
    let uik_sums = {
        let mut stmt = db.conn.prepare(
            "SELECT p.tik_id, v.line_number, SUM(v.value)
             FROM precincts p
             JOIN protocols pr ON pr.election_id=p.election_id AND pr.uik_id=p.uik_id
             JOIN protocol_values v ON v.version_id=pr.current_version_id
             WHERE p.election_id=?1 AND pr.is_strict_valid=1 AND p.tik_id IS NOT NULL
             GROUP BY p.tik_id, v.line_number",
        )?;
        stmt.query_map([CAMPAIGN_ID], |row| {
            Ok((
                (row.get::<_, String>(0)?, row.get::<_, u16>(1)?),
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?
    };
    for (index, (tik_id, total, completed)) in tiks.into_iter().enumerate() {
        if filters.limit.is_some_and(|limit| index >= limit) {
            break;
        }
        let Some(tik_id) = tik_id else {
            summary.incomplete_tiks_skipped += 1;
            continue;
        };
        if !is_complete_tik(total, completed) {
            summary.incomplete_tiks_skipped += 1;
            continue;
        }
        let (report, raw) = api.protocol(&tik_id).await?;
        let (parsed, issues) = parse_report(&report);
        let validation = validate_protocol(&parsed, issues);
        if !validation.strict_valid {
            tracing::warn!(tik=%tik_id, "агрегат ТИК не прошёл структурную проверку");
            continue;
        }
        db.save_aggregate_source(
            CAMPAIGN_ID,
            &tik_id,
            &api.report_url(&tik_id),
            &parsed.report_id,
            &raw,
        )?;
        for line in 1_u16..=22 {
            let official = parsed.lines.get(&line).and_then(|value| value.value);
            let uik_sum = uik_sums.get(&(tik_id.clone(), line)).copied();
            let difference = aggregate_difference(uik_sum, official);
            if difference.is_some_and(|value| value != 0) {
                summary.differing_lines += 1;
            }
            db.conn.execute(
                "INSERT INTO aggregate_checks(election_id,commission_id,line_number,
                   precinct_count,completed_count,uik_sum,official_value,difference,checked_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
                 ON CONFLICT(election_id,commission_id,line_number) DO UPDATE SET
                   precinct_count=excluded.precinct_count,completed_count=excluded.completed_count,
                   uik_sum=excluded.uik_sum,official_value=excluded.official_value,
                   difference=excluded.difference,checked_at=excluded.checked_at",
                rusqlite::params![
                    CAMPAIGN_ID,
                    tik_id,
                    line,
                    total,
                    completed,
                    uik_sum,
                    official,
                    difference,
                    now()
                ],
            )?;
        }
        summary.complete_tiks_checked += 1;
    }
    Ok(summary)
}

#[cfg(test)]
mod aggregate_tests {
    use super::{aggregate_difference, is_complete_tik};

    #[test]
    fn independent_official_value_detects_mutation() {
        assert_eq!(aggregate_difference(Some(101), Some(100)), Some(1));
        assert_eq!(aggregate_difference(Some(100), Some(100)), Some(0));
    }

    #[test]
    fn incomplete_tik_is_not_complete() {
        assert!(!is_complete_tik(10, 9));
        assert!(is_complete_tik(10, 10));
    }
}
