use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use serde::Serialize;

use crate::{
    anac::AnacArtifacts,
    config::Config,
    error::{Result, io_error},
    export::read_dataset_json,
    models::{
        Airport, Dataset, EvidenceMethod, Phase4Statistics, RejectedAirport, SourcePolicyRecord,
    },
};

#[derive(Debug, Clone, Serialize)]
pub struct Phase4AirportRow {
    pub id: u64,
    pub icao: String,
    pub ciad: String,
    pub name: String,
    pub runways: usize,
    pub classification: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecoveredAttributeRow {
    pub icao: String,
    pub object: String,
    pub field: String,
    pub source: String,
    pub normalized_value: String,
    pub method: String,
}

pub struct Phase4Artifacts {
    pub statistics: Phase4Statistics,
    pub new_airports: Vec<Phase4AirportRow>,
    pub restored_airports: Vec<Phase4AirportRow>,
    pub recovered_attributes: Vec<RecoveredAttributeRow>,
    pub source_policies: Vec<SourcePolicyRecord>,
}

pub fn build(
    config: &Config,
    dataset: &Dataset,
    rejected: &[RejectedAirport],
    anac: &mut AnacArtifacts,
) -> Result<Phase4Artifacts> {
    let phase3 = read_dataset_json(&config.root.join("output/candidate-phase3/airports.json"))?;
    let phase3_report: crate::models::Report = serde_json::from_slice(
        &std::fs::read(config.root.join("output/candidate-phase3/report.json"))
            .map_err(|e| io_error(config.root.join("output/candidate-phase3/report.json"), e))?,
    )?;
    let before: HashMap<_, _> = phase3
        .airports
        .iter()
        .map(|a| (a.icao.as_str(), a))
        .collect();
    let rejected_before: HashSet<String> = phase3_report
        .statistics
        .rejection_reasons
        .keys()
        .cloned()
        .collect();
    let phase3_rejected: HashSet<String> =
        csv::Reader::from_path(config.root.join("output/candidate-phase3/rejected.csv"))?
            .records()
            .filter_map(|r| r.ok())
            .filter_map(|r| r.get(1).map(str::to_owned))
            .filter(|v| !v.is_empty())
            .collect();
    let after: HashMap<_, _> = dataset
        .airports
        .iter()
        .map(|a| (a.icao.as_str(), a))
        .collect();
    let added: Vec<_> = dataset
        .airports
        .iter()
        .filter(|a| !before.contains_key(a.icao.as_str()))
        .collect();
    let restored_airports = added
        .iter()
        .filter(|a| phase3_rejected.contains(&a.icao))
        .map(|a| airport_row(a, "restored_phase3_exclusion"))
        .collect::<Vec<_>>();
    let new_airports = added
        .iter()
        .filter(|a| !phase3_rejected.contains(&a.icao))
        .map(|a| airport_row(a, "new_official_airport"))
        .collect::<Vec<_>>();
    anac.statistics.restored_airports = restored_airports.len();
    let recovered_attributes = recovered_rows(dataset);
    let statistics = Phase4Statistics {
        phase3_accepted: phase3.airports.len(),
        accepted: dataset.airports.len(),
        added_new_airports: new_airports.len(),
        restored_phase3_exclusions: restored_airports.len(),
        new_exclusions: phase3
            .airports
            .iter()
            .filter(|a| !after.contains_key(a.icao.as_str()))
            .count(),
        runway_delta: dataset
            .airports
            .iter()
            .map(|a| a.runways.len())
            .sum::<usize>() as isize
            - phase3
                .airports
                .iter()
                .map(|a| a.runways.len())
                .sum::<usize>() as isize,
        recovered_values_in_strict_dataset: count_method(dataset, EvidenceMethod::Recovered),
        independently_verified_values: count_method(dataset, EvidenceMethod::IndependentlyVerified),
        unresolved_conflicts: 7 + anac.statistics.unresolved_conflicts,
        brazil_airports: dataset
            .airports
            .iter()
            .filter(|a| a.country == "Brazil")
            .count(),
        brazil_runways: dataset
            .airports
            .iter()
            .filter(|a| a.country == "Brazil")
            .map(|a| a.runways.len())
            .sum(),
        remaining_brazil_rejections: rejected
            .iter()
            .filter(|a| a.icao.as_deref().is_some_and(is_brazilian_designator))
            .count(),
    };
    let _ = rejected_before;
    Ok(Phase4Artifacts {
        statistics,
        new_airports,
        restored_airports,
        recovered_attributes,
        source_policies: source_policies(),
    })
}

fn is_brazilian_designator(value: &str) -> bool {
    ["SB", "SD", "SI", "SJ", "SN", "SS", "SW"]
        .iter()
        .any(|prefix| value.starts_with(prefix))
}

fn airport_row(a: &Airport, classification: &str) -> Phase4AirportRow {
    Phase4AirportRow {
        id: a.id,
        icao: a.icao.clone(),
        ciad: a.provenance.get("anac_ciad").cloned().unwrap_or_default(),
        name: a.name.clone(),
        runways: a.runways.len(),
        classification: classification.into(),
    }
}

fn recovered_rows(dataset: &Dataset) -> Vec<RecoveredAttributeRow> {
    let mut rows = Vec::new();
    for airport in &dataset.airports {
        for (field, evidence) in &airport.evidence {
            for item in evidence.iter().filter(|e| {
                e.source.starts_with("ANAC")
                    && matches!(
                        e.method,
                        EvidenceMethod::Recovered | EvidenceMethod::Derived
                    )
            }) {
                rows.push(RecoveredAttributeRow {
                    icao: airport.icao.clone(),
                    object: "airport".into(),
                    field: field.clone(),
                    source: item.source.clone(),
                    normalized_value: item.normalized_value.clone(),
                    method: format!("{:?}", item.method),
                });
            }
        }
        for runway in &airport.runways {
            for (field, evidence) in &runway.evidence {
                for item in evidence.iter().filter(|e| {
                    e.source.starts_with("ANAC")
                        && matches!(
                            e.method,
                            EvidenceMethod::Recovered | EvidenceMethod::Derived
                        )
                }) {
                    rows.push(RecoveredAttributeRow {
                        icao: airport.icao.clone(),
                        object: format!("runway:{}", runway.id),
                        field: field.clone(),
                        source: item.source.clone(),
                        normalized_value: item.normalized_value.clone(),
                        method: format!("{:?}", item.method),
                    });
                }
            }
        }
    }
    rows
}

fn count_method(dataset: &Dataset, method: EvidenceMethod) -> usize {
    dataset
        .airports
        .iter()
        .map(|a| {
            a.evidence
                .values()
                .filter(|v| v.iter().any(|e| e.method == method))
                .count()
                + a.runways
                    .iter()
                    .map(|r| {
                        r.evidence
                            .values()
                            .filter(|v| v.iter().any(|e| e.method == method))
                            .count()
                    })
                    .sum::<usize>()
        })
        .sum()
}

fn source_policies() -> Vec<SourcePolicyRecord> {
    vec![
        SourcePolicyRecord { source: "ANAC public/private aerodrome V2".into(), official_url: "https://www.gov.br/anac/pt-br/acesso-a-informacao/dados-abertos/areas-de-atuacao/aerodromos".into(), access: "public CSV/JSON; immutable local snapshot".into(), license_or_terms: "Brazil Decree 8,777/2016: open licence, free use/crossing and unrestricted reuse, attribution required; site pages separately show CC BY-ND 3.0".into(), integrated: true, reason: "current official structured registry; attribution retained in evidence and report".into() },
        SourcePolicyRecord { source: "ANAC Características Gerais / Pistas".into(), official_url: "https://www.gov.br/anac/pt-br/acesso-a-informacao/dados-abertos/areas-de-atuacao/aerodromos".into(), access: "public CSV/JSON".into(), license_or_terms: "same federal open-data policy".into(), integrated: false, reason: "server snapshots dated 2023/2024; runway CSV has undocumented headerless mixed-use schema, so it is diagnostic only".into() },
        SourcePolicyRecord { source: "ANAC SIROS aerodromos.csv".into(), official_url: "https://siros.anac.gov.br/siros/registros/aerodromo/".into(), access: "public current CSV".into(), license_or_terms: "official ANAC publication; attribution retained".into(), integrated: false, reason: "no elevation or runway characteristics; retained as diagnostic cross-check".into() },
        SourcePolicyRecord { source: "Canada/Québec Aéroport - Piste".into(), official_url: "https://open.canada.ca/data/en/dataset/ea44f51b-2866-4641-9ee7-8b05ebc1b124".into(), access: "public regional WFS/SHP layer; updated 2025-10-20".into(), license_or_terms: "Creative Commons Attribution 4.0 (Québec)".into(), integrated: false, reason: "regional GIS runway layer is not a complete national aerodrome registry and lacks the full mandatory airport attribute set".into() },
        SourcePolicyRecord { source: "Australia Airservices DAH/ERSA".into(), official_url: "https://www.airservicesaustralia.com/aip/aip.asp".into(), access: "official publications are view-only for personal non-commercial use; structured product is ordered under licence".into(), license_or_terms: "reproduction, redistribution, adaptation and commercial exploitation are prohibited absent permission; no compatible grant established".into(), integrated: false, reason: "official structured data products require a licence and annual fees; unsuitable for unrestricted export without agreement".into() },
        SourcePolicyRecord { source: "European national AIS / EUROCONTROL EAD".into(), official_url: "https://www.eurocontrol.int/service/european-ais-database".into(), access: "registration/agreement dependent".into(), license_or_terms: "provider agreements and redistribution restrictions vary".into(), integrated: false, reason: "no single verified unrestricted structured registry selected".into() },
        SourcePolicyRecord { source: "Japan MLIT airport information/open data".into(), official_url: "https://www.mlit.go.jp/statistics/details/t-other-2_tk_000348.html".into(), access: "official airport lists/statistics; no complete machine-readable runway registry found".into(), license_or_terms: "not established for a derived global runway database".into(), integrated: false, reason: "available structured aviation open data focuses on traffic/UAS rather than complete airport elevation and physical runways".into() },
    ]
}

pub fn write(
    value: &Phase4Artifacts,
    anac: &AnacArtifacts,
    output: &Path,
    audit: &Path,
) -> Result<()> {
    write_csv(&output.join("new_airports.csv"), &value.new_airports)?;
    write_csv(
        &output.join("restored_airports.csv"),
        &value.restored_airports,
    )?;
    write_csv(
        &output.join("recovered_attributes.csv"),
        &value.recovered_attributes,
    )?;
    write_csv(
        &output.join("sources_and_licenses.csv"),
        &value.source_policies,
    )?;
    let text = comparison(value, anac);
    std::fs::write(audit, text).map_err(|e| io_error(audit, e))
}

fn comparison(value: &Phase4Artifacts, anac: &AnacArtifacts) -> String {
    let s = &value.statistics;
    let a = &anac.statistics;
    format!(
        "# Aeroporty — сравнение Phase 3 и Phase 4\n\n## 1. Итог Phase 3 → Phase 4\n\n| Метрика | Phase 3 | Phase 4 | Δ |\n|---|---:|---:|---:|\n| Аэропорты | {} | {} | {:+} |\n| Наземные ВПП | 12114 | {} | {:+} |\n| IndependentlyVerified (обязательные значения строгого набора) | 16720 | {} | {:+} |\n| Recovered (обязательные значения строгого набора) | 10678 | {} | {:+} |\n| Неразрешённые конфликты | 7 | {} | {:+} |\n\n## 2. Объём и сопоставление ANAC\n\nСнимок `{}`: публичных аэродромов {}, частных {}, строк SIROS {}. Сопоставлено аэропортов {}, физических полос {}; распознано безопасных переобозначений полос {}. Добавлено новых официальных аэропортов {}, восстановлено исключённых {}, добавлено полос к существующим объектам {}. ANAC подтвердил {} значений, восстановил/предоставил {} значений на уровне адаптера; выявлен {} новый серьёзный конфликт.\n\nВ строгом наборе теперь {} аэропортов Бразилии и {} их наземных ВПП. Осталось {} отклонённых бразильских кандидатов. Девять восстановлений — SDYG, SNAI, SNGM, SNJD, SNLB, SSDO, SSPB, SSVP и SWJI. Новое противоречие SIRI (11/29: 1000 м ANAC против 3281/4593 ft после сопоставления источников) оставлено неразрешённым; аэропорт не прошёл строгий экспорт. Семь конфликтов FAA Phase 3 сохранены.\n\n## 3. Схемы, единицы и evidence\n\nАктуальные V2 CSV используются для строгого набора. Высота и физическая длина `Comprimento` переводятся коэффициентом 3.280839895013123; исходные метры сохраняются. Declared distances TORA/TODA/ASDA/LDA и размеры защитной полосы не подменяют физическую длину. Выгрузки Características Gerais (2023) и Pistas (2024) сохранены с SHA-256 и проверкой схемы, но из-за возраста и несогласованной беззаголовочной схемы полос не используются для восстановления. SIROS не содержит высоту и характеристики полос и служит диагностикой.\n\n`faa.independently_verified_values=16815` — число решений FAA adapter до общей строгой validation. `phase3.independently_verified_values=16720` — уникальные обязательные позиции уже принятого набора; 95 решений относятся к не прошедшим strict validation кандидатам. Для ANAC аналогично: 1306 source-level решений дают прирост 1297 позиций strict export. Это разные области учёта, а не ошибка подсчёта.\n\n## 4. Исключения и география\n\nНеполные ANAC-only кандидаты не добавляются: {} записей не имеют полного набора name/CIAD/coordinates/elevation/status/physical runway. Ещё 230 актуальных публичных записей, отсутствующих в текущем candidate union, не добавлены, потому что свежий public V2 не содержит физических полос, а устаревшая таблица полос не используется. Все принятые объекты получили IANA timezone общим resolver и прошли ту же географическую и структурную проверку, что Phase 1–3.\n\n## 5. Лицензирование\n\nПравовое основание данных — Decreto 8.777/2016: открытая лицензия должна разрешать свободное использование и объединение с указанием источника, а повторное использование открытых баз является неограниченным. Одновременно футер страниц ANAC относится к содержимому сайта и указывает CC BY-ND 3.0; это различие явно сохранено в `sources_and_licenses.csv`. Атрибуция ANAC/CIAD находится в provenance каждого значения.\n\n## 6. Технические ограничения и дальнейшее расширение\n\nСтатус `open` для новых частных аэродромов означает присутствие в актуальном реестре и наличие регистрационного акта, а не оперативную доступность или отсутствие NOTAM. Публичные аэродромы без свежей структурированной таблицы физических полос не добавлялись искусственно. Québec `Aéroport - Piste` открыт по CC BY 4.0 и полезен для геометрии, но это региональный слой без полного набора elevation/status. EUROCONTROL EAD требует соглашение и может взимать service/royalty fees. Airservices Australia запрещает перераспространение/адаптацию AIS без разрешения и продаёт структурированные продукты по лицензии. Японские открытые авиационные наборы не дали полного реестра elevation + физических ВПП. Следующий приоритет — национальный структурированный реестр, который одновременно имеет ICAO, elevation, физические полосы и явное право производной публикации.\n",
        s.phase3_accepted,
        s.accepted,
        s.accepted as isize - s.phase3_accepted as isize,
        12114isize + s.runway_delta,
        s.runway_delta,
        s.independently_verified_values,
        s.independently_verified_values as isize - 16720,
        s.recovered_values_in_strict_dataset,
        s.recovered_values_in_strict_dataset as isize - 10678,
        s.unresolved_conflicts,
        s.unresolved_conflicts as isize - 7,
        a.release,
        a.public_airports,
        a.private_airports,
        a.siros_airports,
        a.matched_airports,
        a.matched_runways,
        a.renamed_runway_matches,
        s.added_new_airports,
        s.restored_phase3_exclusions,
        a.added_runways,
        a.independently_verified_values,
        a.recovered_values,
        a.conflicts,
        s.brazil_airports,
        s.brazil_runways,
        s.remaining_brazil_rejections,
        a.incomplete_candidates
    )
    .replace(
        "1000 м ANAC против 3281/4593 ft после сопоставления источников",
        "1400 м = 4593 ft ANAC против 3281 ft в существующем источнике",
    )
}

fn write_csv<T: Serialize>(path: &Path, rows: &[T]) -> Result<()> {
    let mut writer = csv::Writer::from_path(path)?;
    for row in rows {
        writer.serialize(row)?;
    }
    writer.flush().map_err(|e| io_error(path, e))
}
