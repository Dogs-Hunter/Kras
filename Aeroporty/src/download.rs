use std::{path::Path, time::Duration};

use chrono::Utc;
use reqwest::{Client, StatusCode, header};
use serde::{Deserialize, Serialize};
use tokio::time::sleep;
use tracing::{info, warn};

use crate::{
    config::Config,
    error::{Result, io_error},
    models::SourceInfo,
    sources::ourairports::{SourceFile, source_files},
};

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheMetadata {
    retrieved_at: Option<String>,
    etag: Option<String>,
    last_modified: Option<String>,
}

pub async fn update_sources(config: &Config) -> Result<Vec<SourceInfo>> {
    tokio::fs::create_dir_all(&config.cache_dir)
        .await
        .map_err(|e| io_error(&config.cache_dir, e))?;
    let client = Client::builder()
        .user_agent(concat!("aeroporty/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .build()?;
    let mut result = Vec::new();
    for source in source_files(&config.cache_dir) {
        result.push(update_one(&client, config, &source).await?);
    }
    Ok(result)
}

/// Read the pinned OurAirports snapshot without mutating it. Full dataset runs
/// use this path; an explicit `update` command remains the only way to refresh.
pub async fn cached_sources(config: &Config) -> Result<Vec<SourceInfo>> {
    let mut pinned = config.clone();
    pinned.offline = true;
    update_sources(&pinned).await
}

async fn update_one(client: &Client, config: &Config, source: &SourceFile) -> Result<SourceInfo> {
    let meta_path = source.path.with_extension("csv.meta.json");
    let old_meta: CacheMetadata = read_metadata(&meta_path).await.unwrap_or_default();
    if config.offline {
        validate_cached_csv(&source.path).await?;
        return Ok(source_info(source, old_meta));
    }

    let mut last_error = None;
    for attempt in 0..3_u32 {
        let mut request = client.get(&source.url);
        if !config.force {
            if let Some(etag) = &old_meta.etag {
                request = request.header(header::IF_NONE_MATCH, etag);
            }
            if let Some(modified) = &old_meta.last_modified {
                request = request.header(header::IF_MODIFIED_SINCE, modified);
            }
        }
        match request.send().await {
            Ok(response) if response.status() == StatusCode::NOT_MODIFIED => {
                validate_cached_csv(&source.path).await?;
                info!(file = source.name, "кэш актуален");
                return Ok(source_info(source, old_meta));
            }
            Ok(response) if response.status().is_success() => {
                let etag = response
                    .headers()
                    .get(header::ETAG)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                let last_modified = response
                    .headers()
                    .get(header::LAST_MODIFIED)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                let bytes = response.bytes().await?;
                if bytes.is_empty() {
                    return Err(crate::Error::Data(format!(
                        "источник {} вернул пустой файл",
                        source.url
                    )));
                }
                let part = source.path.with_extension("csv.part");
                tokio::fs::write(&part, &bytes)
                    .await
                    .map_err(|e| io_error(&part, e))?;
                validate_cached_csv(&part).await?;
                tokio::fs::copy(&part, &source.path)
                    .await
                    .map_err(|e| io_error(&source.path, e))?;
                tokio::fs::remove_file(&part)
                    .await
                    .map_err(|e| io_error(&part, e))?;
                let metadata = CacheMetadata {
                    retrieved_at: Some(Utc::now().to_rfc3339()),
                    etag,
                    last_modified,
                };
                let json = serde_json::to_vec_pretty(&metadata)?;
                tokio::fs::write(&meta_path, json)
                    .await
                    .map_err(|e| io_error(&meta_path, e))?;
                info!(file = source.name, bytes = bytes.len(), "файл обновлён");
                return Ok(source_info(source, metadata));
            }
            Ok(response) => {
                last_error = Some(format!("HTTP {} для {}", response.status(), source.url));
            }
            Err(error) => last_error = Some(error.to_string()),
        }
        if attempt < 2 {
            let delay = Duration::from_secs(2_u64.pow(attempt));
            warn!(
                file = source.name,
                attempt = attempt + 1,
                ?delay,
                "повтор загрузки"
            );
            sleep(delay).await;
        }
    }
    if source.path.exists() && !config.force {
        warn!(
            file = source.name,
            "сеть недоступна, используется проверенный кэш"
        );
        validate_cached_csv(&source.path).await?;
        Ok(source_info(source, old_meta))
    } else {
        Err(crate::Error::Data(last_error.unwrap_or_else(|| {
            format!("не удалось загрузить {}", source.url)
        })))
    }
}

async fn read_metadata(path: &Path) -> Result<CacheMetadata> {
    let bytes = tokio::fs::read(path).await.map_err(|e| io_error(path, e))?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub async fn validate_cached_csv(path: &Path) -> Result<()> {
    if !path.exists() {
        return Err(crate::Error::MissingCache(path.to_path_buf()));
    }
    let bytes = tokio::fs::read(path).await.map_err(|e| io_error(path, e))?;
    if bytes.len() < 16 || !bytes.contains(&b'\n') || !bytes.contains(&b',') {
        return Err(crate::Error::Data(format!(
            "кэш {} пуст или не похож на CSV",
            path.display()
        )));
    }
    let file_name = path
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or_default();
    let required: &[&str] = if file_name.starts_with("airports") {
        &[
            "id",
            "ident",
            "type",
            "name",
            "latitude_deg",
            "longitude_deg",
            "elevation_ft",
            "iso_country",
            "icao_code",
        ]
    } else if file_name.starts_with("runways") {
        &[
            "id",
            "airport_ref",
            "length_ft",
            "surface",
            "le_ident",
            "he_ident",
        ]
    } else if file_name.starts_with("countries") {
        &["code", "name"]
    } else {
        return Err(crate::Error::Data(format!(
            "неизвестный тип CSV-кэша {}",
            path.display()
        )));
    };
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(bytes.as_slice());
    let headers = reader.headers()?.clone();
    let missing: Vec<_> = required
        .iter()
        .filter(|field| !headers.iter().any(|header| header == **field))
        .copied()
        .collect();
    if !missing.is_empty() {
        return Err(crate::Error::Data(format!(
            "кэш {} не соответствует ожидаемой схеме; отсутствуют: {}",
            path.display(),
            missing.join(", ")
        )));
    }
    if reader.records().next().transpose()?.is_none() {
        return Err(crate::Error::Data(format!(
            "кэш {} не содержит строк данных",
            path.display()
        )));
    }
    Ok(())
}

fn source_info(source: &SourceFile, metadata: CacheMetadata) -> SourceInfo {
    SourceInfo {
        name: format!("OurAirports/{}", source.name),
        url: source.url.clone(),
        local_file: source.path.display().to_string(),
        retrieved_at: metadata.retrieved_at,
        etag: metadata.etag,
        last_modified: metadata.last_modified,
    }
}
