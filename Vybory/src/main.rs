use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};
use elections::{
    DEFAULT_API_BASE,
    api::OfficialApi,
    db::Database,
    export::{export_all, statistics},
    pipeline::{Filters, discover, run_downloads, verify_aggregates},
    validation::revalidate_database,
};
use tokio::sync::Mutex;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "elections",
    version,
    about = "Сбор официальных протоколов № 2 УИК выборов Госдумы 2026 года"
)]
struct Cli {
    #[arg(long, default_value = "database/elections.sqlite", global = true)]
    database: PathBuf,
    #[arg(long, default_value = DEFAULT_API_BASE, global = true)]
    api_base: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Исследовать кампанию, подтвердить схему и обнаружить комиссии/УИК.
    Discover(NetworkArgs),
    /// Загрузить новые протоколы из очереди.
    Run(RunArgs),
    /// Продолжить загрузку, включая ранее неуспешные задания.
    Resume(RunArgs),
    /// Повторно проверить сохранённые протоколы без сети.
    Validate,
    /// Экспортировать строгий датасет и отчёты из SQLite.
    Export(OutputArgs),
    /// Показать фактическую статистику SQLite.
    Stats,
    /// Сверить полные наборы УИК с официальными агрегатами ТИК.
    Verify(VerifyArgs),
}

#[derive(Debug, Clone, Args, Default)]
struct FilterArgs {
    #[arg(long)]
    federal_district: Option<String>,
    #[arg(long)]
    region: Option<String>,
    #[arg(long)]
    tik: Option<String>,
    #[arg(long)]
    uik: Option<i64>,
    #[arg(long)]
    limit: Option<usize>,
}

impl From<FilterArgs> for Filters {
    fn from(value: FilterArgs) -> Self {
        Self {
            federal_district: value.federal_district,
            region: value.region,
            tik: value.tik,
            uik: value.uik,
            limit: value.limit,
        }
    }
}

#[derive(Debug, Clone, Args)]
struct NetworkArgs {
    #[command(flatten)]
    filters: FilterArgs,
    /// Максимальное среднее число HTTP-запросов в секунду.
    #[arg(long, default_value_t = 10.0)]
    rate_limit: f64,
    /// Запретить сетевые запросы.
    #[arg(long)]
    offline: bool,
    /// Совместимый флаг; WebDriver основному клиенту не требуется.
    #[arg(long, default_value_t = true)]
    headless: bool,
    /// Каталог результатов внутри проекта.
    #[arg(long, default_value = "output/2026")]
    output: PathBuf,
}

#[derive(Debug, Clone, Args)]
struct RunArgs {
    #[command(flatten)]
    network: NetworkArgs,
    #[arg(long, default_value_t = 8)]
    concurrency: usize,
    /// Завершённые протоколы не перезаписываются: история остаётся идемпотентной.
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Clone, Args)]
struct OutputArgs {
    #[arg(long, default_value = "output/2026")]
    output: PathBuf,
}

#[derive(Debug, Clone, Args)]
struct VerifyArgs {
    #[command(flatten)]
    network: NetworkArgs,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
    let cli = Cli::parse();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let db_path = path_inside(&root, &cli.database)?;
    let mut db = Database::open(&db_path)?;
    match cli.command {
        Command::Discover(args) => {
            require_online(args.offline, "discover")?;
            note_transport(args.headless);
            let output = path_inside(&root, &args.output)?;
            let api = OfficialApi::new(cli.api_base, args.rate_limit)?;
            let summary = discover(&api, &mut db, &args.filters.into(), &output).await?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        Command::Run(args) => {
            execute_run(args, false, cli.api_base, root, db).await?;
        }
        Command::Resume(args) => {
            execute_run(args, true, cli.api_base, root, db).await?;
        }
        Command::Validate => {
            let (valid, invalid) = revalidate_database(&mut db)?;
            println!(
                "Проверено: {}; валидных: {valid}; отклонено: {invalid}",
                valid + invalid
            );
        }
        Command::Export(args) => {
            let output = path_inside(&root, &args.output)?;
            let report = export_all(&db, &output)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Stats => {
            println!("{}", serde_json::to_string_pretty(&statistics(&db)?)?);
        }
        Command::Verify(args) => {
            require_online(args.network.offline, "verify")?;
            note_transport(args.network.headless);
            let api = OfficialApi::new(cli.api_base, args.network.rate_limit)?;
            let summary = verify_aggregates(&api, &mut db, &args.network.filters.into()).await?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
    }
    Ok(())
}

async fn execute_run(
    args: RunArgs,
    include_failed: bool,
    api_base: String,
    root: PathBuf,
    mut db: Database,
) -> Result<()> {
    require_online(args.network.offline, "run/resume")?;
    note_transport(args.network.headless);
    if args.force {
        tracing::warn!(
            "--force не перезаписывает завершённые протоколы; официальный report ID сохраняет идемпотентность"
        );
    }
    if db.count("precincts")? == 0 {
        let api = OfficialApi::new(api_base.clone(), args.network.rate_limit)?;
        let output = path_inside(&root, &args.network.output)?;
        discover(&api, &mut db, &args.network.filters.clone().into(), &output).await?;
    }
    let api = OfficialApi::new(api_base, args.network.rate_limit)?;
    let cache = path_inside(&root, Path::new("cache/raw/protocols"))?;
    let shared = Arc::new(Mutex::new(db));
    let summary = run_downloads(
        api,
        shared,
        args.network.filters.into(),
        args.concurrency,
        include_failed,
        cache,
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn note_transport(headless: bool) {
    if !headless {
        tracing::info!(
            "основной источник доступен как JSON API; браузерный режим не требуется и не запускается"
        );
    }
}

fn require_online(offline: bool, command: &str) -> Result<()> {
    if offline {
        bail!(
            "команда {command} требует сети; для автономной работы используйте validate/export/stats"
        );
    }
    Ok(())
}

fn path_inside(root: &Path, requested: &Path) -> Result<PathBuf> {
    if requested
        .components()
        .any(|component| component == Component::ParentDir)
    {
        bail!(
            "путь не должен выходить за пределы проекта: {}",
            requested.display()
        );
    }
    let candidate = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };
    if !candidate.starts_with(root) {
        bail!("путь должен находиться внутри {}", root.display());
    }
    Ok(candidate)
}
