//! MCP over Streamable HTTP, stateless JSON-response mode: every POST carries
//! one JSON-RPC message (or a batch) and gets its answer in the response body.

use std::sync::LazyLock;

use anyhow::{Result, bail};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, body::Bytes};
use regex::Regex;
use serde_json::{Value, json};

use crate::garmin::Garmin;

const SUPPORTED_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

static DATE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}$").unwrap());

pub async fn handle(garmin: &Garmin, body: Bytes) -> Response {
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(rpc_error(Value::Null, -32700, &format!("Parse error: {e}"))),
            )
                .into_response();
        }
    };
    let answer = match msg {
        Value::Array(batch) => {
            let mut out = Vec::new();
            for m in batch {
                if let Some(r) = process(garmin, m).await {
                    out.push(r);
                }
            }
            (!out.is_empty()).then_some(Value::Array(out))
        }
        m => process(garmin, m).await,
    };
    match answer {
        Some(v) => Json(v).into_response(),
        // Only notifications/responses: nothing to answer.
        None => StatusCode::ACCEPTED.into_response(),
    }
}

async fn process(garmin: &Garmin, msg: Value) -> Option<Value> {
    let Some(obj) = msg.as_object() else {
        return Some(rpc_error(Value::Null, -32600, "Invalid Request"));
    };
    let method = obj.get("method").and_then(Value::as_str)?; // a response: ignore
    let id = obj.get("id").cloned()?; // a notification: ignore
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or_default();
            let version = SUPPORTED_VERSIONS
                .iter()
                .find(|v| **v == asked)
                .unwrap_or(&SUPPORTED_VERSIONS[0]);
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "Garmin", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Read-only access to the user's Garmin Connect data \
                    (activities, daily wellness, sleep, heart rate, Body Battery, \
                    stress, HRV). Dates are YYYY-MM-DD."
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tool_list() }),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call_tool(garmin, name, &args).await {
                Ok(v) => json!({
                    "content": [{ "type": "text", "text": v.to_string() }],
                    "isError": false
                }),
                Err(e) => json!({
                    "content": [{ "type": "text", "text": format!("Error executing tool {name}: {e:#}") }],
                    "isError": true
                }),
            }
        }
        _ => {
            return Some(rpc_error(
                id,
                -32601,
                &format!("Method not found: {method}"),
            ));
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn date_tool(name: &str, description: &str) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": { "date": { "type": "string", "description": "YYYY-MM-DD" } },
            "required": ["date"]
        },
        "annotations": { "readOnlyHint": true }
    })
}

fn tool_list() -> Vec<Value> {
    vec![
        json!({
            "name": "get_user_profile",
            "description": "Get the Garmin user profile (name, locale, activity level).",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "get_activities",
            "description": "List recent activities (runs, rides, swims...). Paginate with start/limit.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "default": 10, "minimum": 1 },
                    "start": { "type": "integer", "default": 0, "minimum": 0 }
                }
            },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "get_activity_details",
            "description": "Get full details for one activity by its ID.",
            "inputSchema": {
                "type": "object",
                "properties": { "activity_id": { "type": "integer" } },
                "required": ["activity_id"]
            },
            "annotations": { "readOnlyHint": true }
        }),
        date_tool(
            "get_daily_summary",
            "Daily wellness summary (steps, calories, distance...) for a date (YYYY-MM-DD).",
        ),
        date_tool(
            "get_sleep",
            "Sleep data (stages, score, duration) for a date (YYYY-MM-DD).",
        ),
        date_tool(
            "get_heart_rate",
            "Daily heart rate values for a date (YYYY-MM-DD).",
        ),
        date_tool(
            "get_body_battery",
            "Body Battery report for a date (YYYY-MM-DD).",
        ),
        date_tool("get_stress", "Daily stress levels for a date (YYYY-MM-DD)."),
        date_tool(
            "get_hrv",
            "Heart rate variability (HRV) data for a date (YYYY-MM-DD).",
        ),
    ]
}

/// A non-negative integer argument; numeric strings are accepted too.
fn int_arg(args: &Value, key: &str, default: Option<u64>) -> Result<u64> {
    match args.get(key) {
        None | Some(Value::Null) => match default {
            Some(d) => Ok(d),
            None => bail!("missing required argument `{key}`"),
        },
        Some(Value::Number(n)) if n.as_u64().is_some() => Ok(n.as_u64().unwrap()),
        Some(Value::String(s)) if s.parse::<u64>().is_ok() => Ok(s.parse().unwrap()),
        Some(_) => bail!("`{key}` must be a non-negative integer"),
    }
}

/// Dates end up in URL paths, so anything but YYYY-MM-DD is refused.
fn date_arg(args: &Value) -> Result<String> {
    match args.get("date").and_then(Value::as_str) {
        Some(d) if DATE_RE.is_match(d) => Ok(d.to_string()),
        Some(_) => bail!("`date` must be formatted YYYY-MM-DD"),
        None => bail!("missing required argument `date`"),
    }
}

pub async fn call_tool(g: &Garmin, name: &str, args: &Value) -> Result<Value> {
    match name {
        "get_user_profile" => g.api("/userprofile-service/socialProfile", &[]).await,
        "get_activities" => {
            let limit = int_arg(args, "limit", Some(10))?.max(1);
            let start = int_arg(args, "start", Some(0))?;
            g.api(
                "/activitylist-service/activities/search/activities",
                &[("limit", limit.to_string()), ("start", start.to_string())],
            )
            .await
        }
        "get_activity_details" => {
            let id = int_arg(args, "activity_id", None)?;
            g.api(&format!("/activity-service/activity/{id}"), &[])
                .await
        }
        "get_daily_summary" => {
            let date = date_arg(args)?;
            let who = g.display_name().await?;
            g.api(
                &format!("/usersummary-service/usersummary/daily/{who}"),
                &[("calendarDate", date)],
            )
            .await
        }
        "get_sleep" => {
            let date = date_arg(args)?;
            let who = g.display_name().await?;
            g.api(
                &format!("/wellness-service/wellness/dailySleepData/{who}"),
                &[("date", date), ("nonSleepBufferMinutes", "60".into())],
            )
            .await
        }
        "get_heart_rate" => {
            let date = date_arg(args)?;
            let who = g.display_name().await?;
            g.api(
                &format!("/wellness-service/wellness/dailyHeartRate/{who}"),
                &[("date", date)],
            )
            .await
        }
        "get_body_battery" => {
            let date = date_arg(args)?;
            g.api(
                "/wellness-service/wellness/bodyBattery/reports/daily",
                &[("startDate", date.clone()), ("endDate", date)],
            )
            .await
        }
        "get_stress" => {
            let date = date_arg(args)?;
            g.api(
                &format!("/wellness-service/wellness/dailyStress/{date}"),
                &[],
            )
            .await
        }
        "get_hrv" => {
            let date = date_arg(args)?;
            g.api(&format!("/hrv-service/hrv/{date}"), &[]).await
        }
        _ => bail!("Unknown tool: {name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn initialize_and_list() {
        let g = Garmin::new();
        let init = process(
            &g,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize",
                   "params":{"protocolVersion":"2025-03-26","capabilities":{},
                             "clientInfo":{"name":"t","version":"0"}}}),
        )
        .await
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert!(
            process(
                &g,
                json!({"jsonrpc":"2.0","method":"notifications/initialized"})
            )
            .await
            .is_none()
        );
        let list = process(&g, json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
            .await
            .unwrap();
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 9);
    }

    #[test]
    fn validates_arguments() {
        assert!(date_arg(&json!({"date":"2026-11-26"})).is_ok());
        assert!(date_arg(&json!({"date":"../../userprofile-service"})).is_err());
        assert_eq!(
            int_arg(&json!({"activity_id":"42"}), "activity_id", None).unwrap(),
            42
        );
        assert!(int_arg(&json!({"activity_id":-1}), "activity_id", None).is_err());
        assert_eq!(int_arg(&json!({}), "limit", Some(10)).unwrap(), 10);
    }
}
