use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::env;

const RESOURCE_METADATA: &str = "https://mcp.vkusvill.ru/.well-known/oauth-protected-resource";

#[derive(Debug, Deserialize, Serialize)]
pub struct ProtectedResourceMetadata {
    pub resource: String,
    pub authorization_servers: Vec<String>,
    pub scopes_supported: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AuthorizationServerMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub revocation_endpoint: Option<String>,
    pub grant_types_supported: Vec<String>,
    pub code_challenge_methods_supported: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct AuthStatus {
    pub state: &'static str,
    pub token_present: bool,
    pub registration_url: &'static str,
    pub resource: ProtectedResourceMetadata,
    pub authorization_server: AuthorizationServerMetadata,
    pub note: &'static str,
}

impl AuthStatus {
    pub fn discover(client: &Client) -> Result<Self, String> {
        let resource: ProtectedResourceMetadata = client
            .get(RESOURCE_METADATA)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|error| format!("OAuth resource discovery failed: {error}"))?
            .json()
            .map_err(|error| format!("invalid OAuth resource metadata: {error}"))?;
        let issuer = resource
            .authorization_servers
            .first()
            .ok_or("OAuth resource metadata has no authorization server")?;
        let metadata_url = format!(
            "{}/.well-known/oauth-authorization-server",
            issuer.trim_end_matches('/')
        );
        let authorization_server: AuthorizationServerMetadata = client
            .get(metadata_url)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|error| format!("OAuth server discovery failed: {error}"))?
            .json()
            .map_err(|error| format!("invalid OAuth server metadata: {error}"))?;
        let token_present = env::var("VUSVILL_MCP_ACCESS_TOKEN").is_ok();
        Ok(Self {
            state: if token_present {
                "ExistingTokenConfigured"
            } else {
                "PendingAuthorization"
            },
            token_present,
            registration_url: "https://vkusvill.ru/oauth/registration/",
            resource,
            authorization_server,
            note: "Registration and interactive OAuth are intentionally not performed by this command.",
        })
    }
}
