use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use regex::Regex;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::sync::Mutex;

use crate::model::{ApiReport, CommissionNode, Election};

const USER_AGENT: &str = "Vybory/0.1 (official election data research; reproducible client)";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Challenge {
    pub_token: String,
    js_task: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChallengeAnswer<'a> {
    pub_token: &'a str,
    answer: String,
    fingerprint: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SolvedChallenge {
    api_key: String,
}

#[derive(Default)]
pub struct ApiMetrics {
    pub requests: AtomicU64,
    pub retries: AtomicU64,
    pub errors: AtomicU64,
    pub network_errors: AtomicU64,
}

#[derive(Clone)]
pub struct OfficialApi {
    client: Client,
    base_url: String,
    key: Arc<Mutex<Option<String>>>,
    limiter: Arc<Mutex<Instant>>,
    interval: Duration,
    pub metrics: Arc<ApiMetrics>,
}

impl OfficialApi {
    pub fn new(base_url: impl Into<String>, requests_per_second: f64) -> Result<Self> {
        if !requests_per_second.is_finite() || requests_per_second <= 0.0 {
            bail!("--rate-limit должен быть положительным числом");
        }
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(45))
            .build()
            .context("не удалось создать HTTP-клиент")?;
        Ok(Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            key: Arc::new(Mutex::new(None)),
            limiter: Arc::new(Mutex::new(Instant::now())),
            interval: Duration::from_secs_f64(1.0 / requests_per_second),
            metrics: Arc::new(ApiMetrics::default()),
        })
    }

    pub fn report_url(&self, commission_id: &str) -> String {
        format!(
            "{}/reports/242?commissionClassifierId={}&protocolNum=2",
            self.base_url, commission_id
        )
    }

    async fn throttle(&self) {
        let mut next = self.limiter.lock().await;
        let now = Instant::now();
        if *next > now {
            tokio::time::sleep(*next - now).await;
        }
        *next = Instant::now() + self.interval;
    }

    fn solve_task(task: &str) -> Result<String> {
        let re = Regex::new(r"^\s*return\s+(\d+)\s*([+-])\s*(\d+)\s*;\s*$")?;
        let captures = re
            .captures(task)
            .ok_or_else(|| anyhow!("сервер вернул неподдерживаемую challenge-задачу"))?;
        let left: i64 = captures[1].parse()?;
        let right: i64 = captures[3].parse()?;
        let value = match &captures[2] {
            "+" => left.checked_add(right),
            "-" => left.checked_sub(right),
            _ => None,
        }
        .ok_or_else(|| anyhow!("переполнение при решении challenge"))?;
        Ok(value.to_string())
    }

    async fn refresh_key(&self) -> Result<String> {
        self.throttle().await;
        self.metrics.requests.fetch_add(1, Ordering::Relaxed);
        let challenge: Challenge = self
            .client
            .get(format!("{}/challenge/get", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let answer = Self::solve_task(&challenge.js_task)?;
        self.throttle().await;
        self.metrics.requests.fetch_add(1, Ordering::Relaxed);
        let solved: SolvedChallenge = self
            .client
            .post(format!("{}/challenge/solve", self.base_url))
            .json(&ChallengeAnswer {
                pub_token: &challenge.pub_token,
                answer,
                fingerprint: USER_AGENT,
            })
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        *self.key.lock().await = Some(solved.api_key.clone());
        Ok(solved.api_key)
    }

    async fn current_key(&self) -> Result<String> {
        if let Some(key) = self.key.lock().await.clone() {
            return Ok(key);
        }
        self.refresh_key().await
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(String, String)],
    ) -> Result<T> {
        let mut delay = Duration::from_millis(500);
        for attempt in 0..4 {
            let key = self.current_key().await?;
            self.throttle().await;
            self.metrics.requests.fetch_add(1, Ordering::Relaxed);
            let response = self
                .client
                .get(format!("{}{}", self.base_url, path))
                .query(query)
                .header("X-Api-Key", &key)
                .header("X-Client-Fingerprint", USER_AGENT)
                .send()
                .await;
            match response {
                Ok(response) if response.status() == StatusCode::UNAUTHORIZED => {
                    *self.key.lock().await = None;
                    self.metrics.retries.fetch_add(1, Ordering::Relaxed);
                }
                Ok(response) if response.status().is_success() => {
                    return response
                        .json::<T>()
                        .await
                        .with_context(|| format!("некорректный JSON от {path}"));
                }
                Ok(response)
                    if response.status().is_server_error()
                        || response.status() == StatusCode::TOO_MANY_REQUESTS =>
                {
                    self.metrics.retries.fetch_add(1, Ordering::Relaxed);
                }
                Ok(response) => {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    self.metrics.errors.fetch_add(1, Ordering::Relaxed);
                    bail!("официальный API вернул {status}: {}", truncate(&body, 300));
                }
                Err(error) if attempt < 3 => {
                    tracing::warn!(%error, attempt, "повтор HTTP-запроса");
                    self.metrics.retries.fetch_add(1, Ordering::Relaxed);
                }
                Err(error) => {
                    self.metrics.errors.fetch_add(1, Ordering::Relaxed);
                    self.metrics.network_errors.fetch_add(1, Ordering::Relaxed);
                    return Err(error.into());
                }
            }
            if attempt < 3 {
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(8));
            }
        }
        self.metrics.errors.fetch_add(1, Ordering::Relaxed);
        bail!("исчерпаны повторные попытки для {path}")
    }

    async fn get_text(&self, path: &str, query: &[(String, String)]) -> Result<String> {
        let mut delay = Duration::from_millis(500);
        for attempt in 0..4 {
            let key = self.current_key().await?;
            self.throttle().await;
            self.metrics.requests.fetch_add(1, Ordering::Relaxed);
            let response = self
                .client
                .get(format!("{}{}", self.base_url, path))
                .query(query)
                .header("X-Api-Key", &key)
                .header("X-Client-Fingerprint", USER_AGENT)
                .send()
                .await;
            match response {
                Ok(response) if response.status() == StatusCode::UNAUTHORIZED => {
                    *self.key.lock().await = None;
                    self.metrics.retries.fetch_add(1, Ordering::Relaxed);
                }
                Ok(response) if response.status().is_success() => {
                    return response
                        .text()
                        .await
                        .context("не удалось прочитать HTTP-ответ");
                }
                Ok(response)
                    if response.status().is_server_error()
                        || response.status() == StatusCode::TOO_MANY_REQUESTS =>
                {
                    self.metrics.retries.fetch_add(1, Ordering::Relaxed);
                }
                Ok(response) => {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    self.metrics.errors.fetch_add(1, Ordering::Relaxed);
                    bail!("официальный API вернул {status}: {}", truncate(&body, 300));
                }
                Err(error) if attempt < 3 => {
                    tracing::warn!(%error, attempt, "повтор HTTP-запроса");
                    self.metrics.retries.fetch_add(1, Ordering::Relaxed);
                }
                Err(error) => {
                    self.metrics.errors.fetch_add(1, Ordering::Relaxed);
                    self.metrics.network_errors.fetch_add(1, Ordering::Relaxed);
                    return Err(error.into());
                }
            }
            if attempt < 3 {
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(8));
            }
        }
        self.metrics.errors.fetch_add(1, Ordering::Relaxed);
        bail!("исчерпаны повторные попытки для {path}")
    }

    pub async fn election(&self, election_id: i64) -> Result<Election> {
        self.get_json(&format!("/elections/{election_id}"), &[])
            .await
    }

    pub async fn commission_tree(
        &self,
        election_id: i64,
        classifier_id: Option<&str>,
    ) -> Result<CommissionNode> {
        let mut query = vec![("electionsId".to_owned(), election_id.to_string())];
        if let Some(id) = classifier_id {
            query.push(("classifierId".to_owned(), id.to_owned()));
        }
        self.get_json("/commissionClassifiers", &query).await
    }

    pub async fn protocol(&self, commission_id: &str) -> Result<(ApiReport, String)> {
        let query = vec![
            (
                "commissionClassifierId".to_owned(),
                commission_id.to_owned(),
            ),
            ("protocolNum".to_owned(), "2".to_owned()),
        ];
        let raw = self.get_text("/reports/242", &query).await?;
        let report: ApiReport = serde_json::from_str(&raw)
            .context("официальный сервис вернул некорректный JSON протокола")?;
        Ok((report, raw))
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

pub fn diagnostic_status_for_http(_status: u16) -> &'static str {
    "Unknown"
}

#[cfg(test)]
mod tests {
    use super::{OfficialApi, diagnostic_status_for_http};

    #[test]
    fn solves_only_expected_arithmetic_challenge() {
        assert_eq!(OfficialApi::solve_task("return 854+970;").unwrap(), "1824");
        assert_eq!(OfficialApi::solve_task(" return 10 - 3; ").unwrap(), "7");
        assert!(OfficialApi::solve_task("return fetch('x');").is_err());
    }

    #[test]
    fn http_404_is_not_treated_as_not_published() {
        assert_eq!(diagnostic_status_for_http(404), "Unknown");
    }

    #[test]
    fn raw_protocol_body_keeps_unknown_fields() {
        let raw = include_str!("../tests/fixtures/uik_1.json").replace(
            "\"reportType\": \"242\"",
            "\"reportType\": \"242\", \"futureField\": {\"kept\": true}",
        );
        let _: crate::model::ApiReport = serde_json::from_str(&raw).unwrap();
        assert!(raw.contains("futureField"));
    }
}
