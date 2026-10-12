use std::path::PathBuf;

use aeroporty::{config::Config, export::ExportFormat, pipeline, report::print_stats};
use clap::{Args, Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "aeroporty",
    version,
    about = "Сбор и проверка мирового датасета аэропортов"
)]
struct Cli {
    /// Уровень журналирования: error, warn, info, debug или trace
    #[arg(long, global = true, default_value = "info")]
    log: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Выполнить полный цикл: загрузка, обработка, валидация и экспорт
    Run(ProcessArgs),
    /// Обновить исходные CSV в локальном кэше
    Update(NetworkArgs),
    /// Проверить готовый датасет и эквивалентность форматов
    Validate(OutputArgs),
    /// Повторить экспорт последнего обработанного датасета без загрузки
    Export(ExportArgs),
    /// Показать статистику последней обработки
    Stats(OutputArgs),
    /// Проверить Phase 5 и атомарно опубликовать финальный релиз
    Release(ReleaseArgs),
}

#[derive(Debug, Args)]
struct NetworkArgs {
    /// Не использовать сеть, читать только локальный кэш
    #[arg(long)]
    offline: bool,
    /// Игнорировать условные HTTP-запросы и скачать файлы заново
    #[arg(long)]
    force: bool,
    /// Отключить официальный источник FAA/NASR
    #[arg(long)]
    no_faa: bool,
    /// Дата вступления в силу выпуска FAA/NASR
    #[arg(long, default_value = "2026-10-01")]
    faa_release: String,
    /// Отключить официальный источник ANAC
    #[arg(long)]
    no_anac: bool,
    /// Идентификатор неизменяемого снимка ANAC
    #[arg(long, default_value = "2026-10-11")]
    anac_release: String,
    /// Прочитать локальную официальную выгрузку ANAC из каталога
    #[arg(long)]
    anac_local: Option<PathBuf>,
    /// Отключить Phase 5 и воспроизвести политику Phase 4
    #[arg(long)]
    no_phase5: bool,
}

#[derive(Debug, Args)]
struct OutputArgs {
    /// Подкаталог вывода внутри проекта
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ProcessArgs {
    #[command(flatten)]
    network: NetworkArgs,
    #[command(flatten)]
    output: OutputArgs,
    /// Формат экспорта; аргумент можно повторять (по умолчанию все четыре)
    #[arg(long = "format", value_enum)]
    formats: Vec<ExportFormat>,
    /// Отключить локальное обогащение data/enrichment.json
    #[arg(long)]
    no_enrichment: bool,
}

#[derive(Debug, Args)]
struct ExportArgs {
    #[command(flatten)]
    output: OutputArgs,
    /// Формат экспорта; аргумент можно повторять (по умолчанию все четыре)
    #[arg(long = "format", value_enum)]
    formats: Vec<ExportFormat>,
}

#[derive(Debug, Args)]
struct ReleaseArgs {
    /// Проверенный входной каталог (по умолчанию output/candidate-phase5)
    #[arg(long)]
    input: Option<PathBuf>,
    /// Каталог релиза (по умолчанию output/release)
    #[arg(long)]
    output: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let filter = EnvFilter::try_new(&cli.log).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
    if let Err(error) = execute(cli).await {
        eprintln!("Ошибка: {error}");
        std::process::exit(1);
    }
}

async fn execute(cli: Cli) -> aeroporty::Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    match cli.command {
        Command::Run(args) => {
            let mut config = Config::from_root(root).with_output(args.output.output);
            config.offline = args.network.offline;
            config.force = args.network.force;
            config.enable_local_enrichment = !args.no_enrichment;
            config.enable_faa = !args.network.no_faa;
            config.faa_release = args.network.faa_release;
            config.enable_anac = !args.network.no_anac;
            config.anac_release = args.network.anac_release;
            config.anac_local_dir = args.network.anac_local;
            config.enable_phase5 = !args.network.no_phase5;
            let formats = selected_formats(&args.formats);
            let report = pipeline::run(&config, &formats).await?;
            print_stats(&report.statistics);
            print_faa(report.faa.as_ref());
            print_anac(report.anac.as_ref());
        }
        Command::Update(args) => {
            let mut config = Config::from_root(root);
            config.offline = args.offline;
            config.force = args.force;
            config.enable_faa = !args.no_faa;
            config.faa_release = args.faa_release;
            config.enable_anac = !args.no_anac;
            config.anac_release = args.anac_release;
            config.anac_local_dir = args.anac_local;
            config.enable_phase5 = !args.no_phase5;
            let sources = pipeline::update(&config).await?;
            println!("Проверено источников: {}", sources.len());
        }
        Command::Validate(args) => {
            let config = Config::from_root(root).with_output(args.output);
            let count = pipeline::validate_output(&config)?;
            println!("Валидация пройдена: {count} аэропортов");
        }
        Command::Export(args) => {
            let config = Config::from_root(root).with_output(args.output.output);
            let formats = selected_formats(&args.formats);
            let report = pipeline::export_cached(&config, &formats)?;
            println!("Экспортировано аэропортов: {}", report.statistics.accepted);
        }
        Command::Stats(args) => {
            let config = Config::from_root(root).with_output(args.output);
            let report = pipeline::read_report(&config)?;
            print_stats(&report.statistics);
            print_faa(report.faa.as_ref());
            print_anac(report.anac.as_ref());
        }
        Command::Release(args) => {
            let input = resolve_inside(
                &root,
                args.input
                    .unwrap_or_else(|| PathBuf::from("output/candidate-phase5")),
            );
            let output = resolve_inside(
                &root,
                args.output
                    .unwrap_or_else(|| PathBuf::from("output/release")),
            );
            let manifest = aeroporty::release::create(&root, &input, &output)?;
            println!(
                "Релиз {} готов: {} аэропортов, {} физических ВПП; {}",
                manifest.application_version,
                manifest.airports,
                manifest.physical_runways,
                output.display()
            );
        }
    }
    Ok(())
}

fn resolve_inside(root: &std::path::Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

fn print_anac(stats: Option<&aeroporty::models::AnacStatistics>) {
    if let Some(stats) = stats {
        println!(
            "ANAC {}: public={}, private={}, SIROS={}, matched_airports={}, matched_runways={}, new_airports={}, restored={}, added_runways={}, conflicts={}",
            stats.release,
            stats.public_airports,
            stats.private_airports,
            stats.siros_airports,
            stats.matched_airports,
            stats.matched_runways,
            stats.new_airports_added,
            stats.restored_airports,
            stats.added_runways,
            stats.conflicts
        );
    }
}

fn print_faa(stats: Option<&aeroporty::models::FaaStatistics>) {
    if let Some(stats) = stats {
        println!(
            "FAA/NASR {}: airports={}, with_icao={}, matched_airports={}, matched_runways={}, ambiguous={}",
            stats.release,
            stats.airports,
            stats.airports_with_icao,
            stats.matched_airports,
            stats.matched_runways,
            stats.ambiguous_matches
        );
        println!(
            "FAA evidence: independently_verified={}, recovered={}, conflicts={}, unresolved={}, recovered_airports={}, new_airports={}, new_exclusions={}",
            stats.independently_verified_values,
            stats.recovered_values,
            stats.conflicts,
            stats.unresolved_conflicts,
            stats.recovered_airports,
            stats.new_airports_added,
            stats.new_exclusions
        );
    }
}

fn selected_formats(values: &[ExportFormat]) -> Vec<ExportFormat> {
    if values.is_empty() {
        ExportFormat::ALL.to_vec()
    } else {
        let mut result = values.to_vec();
        result.sort_by_key(|value| match value {
            ExportFormat::Csv => 0,
            ExportFormat::Json => 1,
            ExportFormat::Toml => 2,
            ExportFormat::Yaml => 3,
        });
        result.dedup();
        result
    }
}
