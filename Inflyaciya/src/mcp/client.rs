use reqwest::{
    StatusCode,
    blocking::{Client, Response},
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE},
};
use serde_json::{Value, json};
use std::{env, fmt, thread, time::Duration};

use super::models::{RpcResponse, ToolList};

const PREFERRED_PROTOCOL: &str = "2025-06-18";
const ACCEPT_VALUE: &str = "application/json, text/event-stream";

#[derive(Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Transport,
    Authentication,
    RateLimit,
    Protocol,
    Tool,
    InvalidData,
    Safety,
}

#[derive(Debug)]
pub struct McpError {
    pub kind: ErrorKind,
    pub message: String,
}

impl fmt::Display for McpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

fn error(kind: ErrorKind, message: impl Into<String>) -> String {
    McpError {
        kind,
        message: message.into(),
    }
    .to_string()
}

pub fn classify_status(status: StatusCode) -> ErrorKind {
    match status.as_u16() {
        401 | 403 => ErrorKind::Authentication,
        429 => ErrorKind::RateLimit,
        _ => ErrorKind::Transport,
    }
}

pub fn parse_wire_body(body: &str) -> Result<Value, String> {
    if let Ok(value) = serde_json::from_str(body) {
        return Ok(value);
    }
    let data = body
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .filter(|line| !line.is_empty() && *line != "[DONE]")
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() {
        return Err(error(
            ErrorKind::InvalidData,
            "response is neither JSON nor SSE data",
        ));
    }
    serde_json::from_str(&data)
        .map_err(|parse_error| error(ErrorKind::InvalidData, parse_error.to_string()))
}

pub fn is_read_only_allowed(name: &str) -> bool {
    matches!(
        name,
        "vkusvill_shops"
            | "vkusvill_products_search"
            | "vkusvill_product_details"
            | "vkusvill_product_barcode"
            | "vkusvill_product_analogs"
            | "vkusvill_products_discount"
            | "vkusvill_recipes"
    )
}

pub fn extract_rpc_result(wire: Value) -> Result<Value, String> {
    let rpc: RpcResponse = serde_json::from_value(wire)
        .map_err(|parse_error| error(ErrorKind::Protocol, parse_error.to_string()))?;
    if let Some(rpc_error) = rpc.error {
        return Err(error(
            ErrorKind::Protocol,
            format!(
                "JSON-RPC {}: {}; data={}",
                rpc_error.code, rpc_error.message, rpc_error.data
            ),
        ));
    }
    rpc.result
        .ok_or_else(|| error(ErrorKind::Protocol, "JSON-RPC response has no result"))
}

pub struct McpClient {
    http: Client,
    endpoint: String,
    protocol: String,
    session_id: Option<String>,
    access_token: Option<String>,
    next_id: u64,
    max_retries: u32,
    backoff: Duration,
}

impl McpClient {
    pub fn new(endpoint: &str) -> Result<Self, String> {
        Self::with_policy(endpoint, 30, 2, 750)
    }

    pub fn with_policy(
        endpoint: &str,
        timeout_seconds: u64,
        max_retries: u32,
        backoff_milliseconds: u64,
    ) -> Result<Self, String> {
        let http = Client::builder()
            .timeout(Duration::from_secs(timeout_seconds))
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .map_err(|build_error| error(ErrorKind::Transport, build_error.to_string()))?;
        Ok(Self {
            http,
            endpoint: endpoint.to_owned(),
            protocol: PREFERRED_PROTOCOL.to_owned(),
            session_id: None,
            access_token: env::var("VUSVILL_MCP_ACCESS_TOKEN").ok(),
            next_id: 1,
            max_retries,
            backoff: Duration::from_millis(backoff_milliseconds),
        })
    }

    fn apply_headers(
        &self,
        mut request: reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder {
        request = request
            .header(ACCEPT, ACCEPT_VALUE)
            .header("MCP-Protocol-Version", &self.protocol);
        if let Some(session_id) = &self.session_id {
            request = request.header("Mcp-Session-Id", session_id);
        }
        if let Some(token) = &self.access_token {
            request = request.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        request
    }

    fn handle_response(&mut self, response: Response) -> Result<Value, String> {
        let status = response.status();
        if let Some(value) = response.headers().get("Mcp-Session-Id") {
            self.session_id = value.to_str().ok().map(ToOwned::to_owned);
        }
        let body = response
            .text()
            .map_err(|read_error| error(ErrorKind::Transport, read_error.to_string()))?;
        if !status.is_success() {
            return Err(error(
                classify_status(status),
                format!("HTTP {}: {}", status.as_u16(), body.trim()),
            ));
        }
        if status == StatusCode::ACCEPTED || body.trim().is_empty() {
            return Ok(Value::Null);
        }
        let wire = parse_wire_body(&body)?;
        extract_rpc_result(wire)
    }

    fn post(&mut self, payload: Value) -> Result<Value, String> {
        for attempt in 0..=self.max_retries {
            let request = self
                .http
                .post(&self.endpoint)
                .header(CONTENT_TYPE, "application/json")
                .json(&payload);
            let response = self.apply_headers(request).send();
            match response {
                Ok(response)
                    if (response.status() == StatusCode::TOO_MANY_REQUESTS
                        || response.status().is_server_error())
                        && attempt < self.max_retries =>
                {
                    let retry_after = response
                        .headers()
                        .get("Retry-After")
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.parse::<u64>().ok())
                        .map(Duration::from_secs);
                    thread::sleep(retry_after.unwrap_or_else(|| self.retry_delay(attempt)));
                }
                Ok(response) => return self.handle_response(response),
                Err(_) if attempt < self.max_retries => {
                    thread::sleep(self.retry_delay(attempt));
                }
                Err(send_error) => {
                    return Err(error(ErrorKind::Transport, send_error.to_string()));
                }
            }
        }
        unreachable!("цикл повторов всегда возвращает результат")
    }

    fn retry_delay(&self, attempt: u32) -> Duration {
        let base = self.backoff.saturating_mul(attempt + 1);
        let jitter = Duration::from_millis((self.next_id + u64::from(attempt) * 97) % 251);
        base.saturating_add(jitter)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.post(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
    }

    pub fn initialize(&mut self) -> Result<Value, String> {
        let response = self.request(
            "initialize",
            json!({
                "protocolVersion": PREFERRED_PROTOCOL,
                "capabilities": {},
                "clientInfo": { "name": "inflyaciya-phase06", "version": "0.1.0" }
            }),
        )?;
        if let Some(protocol) = response.get("protocolVersion").and_then(Value::as_str) {
            self.protocol = protocol.to_owned();
        }
        Ok(response)
    }

    pub fn initialized(&mut self) -> Result<(), String> {
        self.post(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))?;
        Ok(())
    }

    pub fn list_tools(&mut self) -> Result<Value, String> {
        self.request("tools/list", json!({}))
    }

    pub fn tool_definitions(&mut self) -> Result<ToolList, String> {
        let value = self.list_tools()?;
        serde_json::from_value(value)
            .map_err(|parse_error| error(ErrorKind::InvalidData, parse_error.to_string()))
    }

    pub fn call_read_only_tool(&mut self, name: &str, arguments: Value) -> Result<Value, String> {
        if !is_read_only_allowed(name) {
            return Err(error(
                ErrorKind::Safety,
                format!("tool {name:?} is not on the read-only allowlist"),
            ));
        }
        let result = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )?;
        if result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(error(ErrorKind::Tool, result.to_string()));
        }
        Ok(result)
    }

    pub fn close(&mut self) -> Result<(), String> {
        let Some(session_id) = self.session_id.take() else {
            return Ok(());
        };
        let response = self
            .apply_headers(
                self.http
                    .delete(&self.endpoint)
                    .header("Mcp-Session-Id", session_id),
            )
            .send()
            .map_err(|send_error| error(ErrorKind::Transport, send_error.to_string()))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(error(
                classify_status(response.status()),
                format!("session DELETE returned HTTP {}", response.status()),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_wire_response() {
        assert_eq!(
            parse_wire_body(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#).unwrap()["id"],
            1
        );
    }

    #[test]
    fn parses_sse_wire_response() {
        let value = parse_wire_body(
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}\n\n",
        )
        .unwrap();
        assert_eq!(value["id"], 2);
    }

    #[test]
    fn classifies_authentication_statuses() {
        assert_eq!(
            classify_status(StatusCode::UNAUTHORIZED),
            ErrorKind::Authentication
        );
        assert_eq!(
            classify_status(StatusCode::FORBIDDEN),
            ErrorKind::Authentication
        );
    }

    #[test]
    fn classifies_rate_limit() {
        assert_eq!(
            classify_status(StatusCode::TOO_MANY_REQUESTS),
            ErrorKind::RateLimit
        );
    }

    #[test]
    fn allows_public_read_only_tools() {
        assert!(is_read_only_allowed("vkusvill_products_search"));
        assert!(is_read_only_allowed("vkusvill_shops"));
    }

    #[test]
    fn blocks_cart_and_personal_tools() {
        assert!(!is_read_only_allowed("vkusvill_cart_link_create"));
        assert!(!is_read_only_allowed("vkusvill_orders_history"));
        assert!(!is_read_only_allowed("vkusvill_product_lp"));
    }

    #[test]
    fn reports_json_rpc_errors_separately() {
        let failure = extract_rpc_result(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {"code": -32602, "message": "invalid id", "data": {"field": "id"}}
        }))
        .unwrap_err();
        assert!(failure.starts_with("Protocol:"));
        assert!(failure.contains("-32602"));
    }
}
