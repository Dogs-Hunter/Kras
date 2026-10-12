use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("ошибка ввода-вывода для {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("ошибка HTTP: {0}")]
    Http(#[from] reqwest::Error),
    #[error("ошибка CSV: {0}")]
    Csv(#[from] csv::Error),
    #[error("ошибка JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("ошибка TOML: {0}")]
    TomlSerialize(#[from] toml::ser::Error),
    #[error("ошибка чтения TOML: {0}")]
    TomlDeserialize(#[from] toml::de::Error),
    #[error("ошибка YAML: {0}")]
    Yaml(#[from] serde_norway::Error),
    #[error("некорректные данные: {0}")]
    Data(String),
    #[error("offline-режим: в кэше отсутствует {0}")]
    MissingCache(PathBuf),
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn io_error(path: impl Into<PathBuf>, source: std::io::Error) -> Error {
    Error::Io {
        path: path.into(),
        source,
    }
}
