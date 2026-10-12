use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct RpcResponse {
    pub result: Option<Value>,
    pub error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ToolList {
    pub tools: Vec<ToolDefinition>,
}

#[derive(Debug, Deserialize)]
pub struct ToolContent {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct ToolResult {
    pub content: Vec<ToolContent>,
    #[serde(rename = "structuredContent", default)]
    pub structured_content: Option<Value>,
    #[serde(rename = "isError", default)]
    pub is_error: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_actual_tool_list_shape() {
        let list: ToolList = serde_json::from_value(serde_json::json!({
            "tools": [{
                "name": "vkusvill_products_search",
                "description": "search",
                "inputSchema": {"type": "object", "properties": {"q": {"type": "string"}}}
            }]
        }))
        .unwrap();
        assert_eq!(list.tools[0].name, "vkusvill_products_search");
        assert_eq!(list.tools[0].input_schema["type"], "object");
    }
}
