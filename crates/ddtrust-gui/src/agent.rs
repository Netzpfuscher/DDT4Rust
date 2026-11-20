use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::Sender;
use ddtrust_core::diag::{DtcReport, Safety};
use ddtrust_core::model::{Database, EcuFile, Protocol};
use serde_json::{json, Value};

use crate::worker::{Command, PollItem};

#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub endpoint: String,
    pub model: String,
    pub api_key: String,
    pub temperature: f32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

impl AgentConfig {
    pub fn from_env() -> Self {
        // DeepSeek is the default provider; any OpenAI-compatible endpoint can override it.
        let base = std::env::var("DDTRUST_LLM_ENDPOINT")
            .ok()
            .or_else(|| std::env::var("DEEPSEEK_BASE_URL").ok())
            .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
            .map(|b| {
                if b.contains("/chat/completions") {
                    b
                } else {
                    format!("{}/chat/completions", b.trim_end_matches('/'))
                }
            })
            .unwrap_or_else(|| "https://api.deepseek.com/chat/completions".into());
        Self {
            endpoint: base,
            model: std::env::var("DDTRUST_LLM_MODEL")
                .or_else(|_| std::env::var("DEEPSEEK_MODEL"))
                .or_else(|_| std::env::var("OPENAI_MODEL"))
                .unwrap_or_else(|_| "deepseek-chat".into()),
            api_key: std::env::var("DDTRUST_LLM_API_KEY")
                .or_else(|_| std::env::var("DEEPSEEK_API_KEY"))
                .or_else(|_| std::env::var("OPENAI_API_KEY"))
                .unwrap_or_default(),
            temperature: 0.2,
        }
    }

    pub fn is_configured(&self) -> bool {
        !self.endpoint.trim().is_empty()
            && (!self.api_key.trim().is_empty()
                || self.endpoint.contains("localhost")
                || self.endpoint.contains("127.0.0.1"))
    }
}

#[derive(Clone, Debug, Default)]
pub struct ContextValue {
    pub name: String,
    pub request: String,
    pub value: Option<String>,
    pub unit: String,
}

pub struct AgentContext {
    pub db: Arc<Database>,
    pub vehicle: Option<String>,
    pub connected: bool,
    pub ecu: Option<Arc<EcuFile>>,
    pub address: String,
    pub group: String,
    pub screen: Option<String>,
    pub screens: Vec<(Option<String>, String)>,
    pub values: Vec<ContextValue>,
    pub dtc: Option<DtcReport>,
}

#[derive(Clone, Debug)]
pub enum AgentEvent {
    Tool(String),
    Assistant(String),
    Error(String),
}

#[derive(Clone, Debug)]
pub enum AgentAction {
    OpenEcu(String),
    OpenScreen(String),
}

pub fn start(
    config: AgentConfig,
    history: Vec<Value>,
    user: String,
    context: AgentContext,
    commands: Sender<Command>,
    actions: Sender<AgentAction>,
    events: Sender<AgentEvent>,
) {
    std::thread::Builder::new()
        .name("ddtrust-agent".into())
        .spawn(move || {
            let result = run(config, history, user, context, commands, actions, &events);
            match result {
                Ok(text) => {
                    let _ = events.send(AgentEvent::Assistant(text));
                }
                Err(e) => {
                    let _ = events.send(AgentEvent::Error(e));
                }
            }
        })
        .ok();
}

fn run(
    config: AgentConfig,
    history: Vec<Value>,
    user: String,
    context: AgentContext,
    commands: Sender<Command>,
    actions: Sender<AgentAction>,
    events: &Sender<AgentEvent>,
) -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| format!("HTTP client: {e}"))?;
    let tools = tool_definitions();
    let mut messages: Vec<Value> = Vec::new();
    messages.push(json!({
        "role": "system",
        "content": SYSTEM_PROMPT,
    }));
    messages.push(json!({
        "role": "system",
        "content": format!(
            "CURRENT DDTRust CONTEXT (read-only snapshot, JSON):\n{}",
            serde_json::to_string_pretty(&context_json(&context)).unwrap_or_default()
        ),
    }));
    messages.extend(history);
    messages.push(json!({"role": "user", "content": user}));

    for _round in 0..6 {
        let body = json!({
            "model": config.model,
            "messages": messages,
            "tools": tools,
            "tool_choice": "auto",
            "temperature": config.temperature,
        });
        let mut request = client.post(config.endpoint.trim()).json(&body);
        if !config.api_key.trim().is_empty() {
            request = request.bearer_auth(config.api_key.trim());
        }
        let response = request
            .send()
            .map_err(|e| format!("Cannot reach the LLM endpoint: {e}"))?;
        let status = response.status();
        let text = response.text().map_err(|e| format!("LLM response: {e}"))?;
        if !status.is_success() {
            return Err(format!(
                "LLM endpoint returned HTTP {status}: {}",
                truncate(&text, 700)
            ));
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| format!("LLM returned invalid JSON: {e}: {}", truncate(&text, 300)))?;
        let message = &value["choices"][0]["message"];
        let content = message["content"]
            .as_str()
            .map(|s| s.to_string())
            .or_else(|| {
                message["content"].as_array().map(|parts| {
                    parts
                        .iter()
                        .filter_map(|p| p["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
            })
            .unwrap_or_default()
            .trim()
            .to_string();
        let tool_calls = message["tool_calls"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if tool_calls.is_empty() {
            if content.is_empty() {
                return Err("The model returned an empty response.".into());
            }
            return Ok(content);
        }
        messages.push(json!({
            "role": "assistant",
            "content": if content.is_empty() { Value::Null } else { Value::String(content.clone()) },
            "tool_calls": tool_calls,
        }));
        for call in tool_calls {
            let name = call["function"]["name"]
                .as_str()
                .unwrap_or("unknown")
                .to_string();
            let args_value = &call["function"]["arguments"];
            let args: Value = if let Some(text) = args_value.as_str() {
                serde_json::from_str(text).unwrap_or_else(|_| json!({}))
            } else if args_value.is_object() {
                args_value.clone()
            } else {
                json!({})
            };
            let id = call["id"].as_str().unwrap_or("tool_call").to_string();
            let result = execute_tool(&name, &args, &context, &commands, &actions);
            let summary = tool_summary(&name, &result);
            let _ = events.send(AgentEvent::Tool(summary));
            messages.push(json!({
                "role": "tool",
                "tool_call_id": id,
                "content": serde_json::to_string(&result).unwrap_or_else(|_| "{\"error\":\"serialization\"}".into()),
            }));
        }
    }
    Err("The agent reached the tool-call limit without a final answer.".into())
}

const SYSTEM_PROMPT: &str = r#"You are DDTRust Agent, an expert Renault/Dacia/Nissan diagnostics assistant built into a CAN/KWP diagnostic application.

You have read-only tools. You cannot write ECU configuration, clear DTCs, run actuators or security access. Never claim you performed such actions. If the user asks for one, explain that it must be done manually in the appropriate screen and require expert confirmation.

Working rules:
- Inspect the supplied CURRENT DDTRust CONTEXT before answering.
- Use tools when you need fresher or more specific data (live values, DTCs, database search, screens).
- Clearly distinguish measured values, stored fault codes, ECU metadata and hypotheses.
- Explain DTCs using the definition data supplied by the ECU/screen context when possible; never invent manufacturer-specific meanings.
- When recommending diagnostics, prioritize: safety, cheap checks (battery/connectors), live-data verification, then parts.
- Be concise and practical. Use short sections or bullets. Include the ECU name, request/data item and units when referring to values.
- The context may be stale. If a value is missing or the app is offline, say so and ask the user to connect/read.
- Use open_screen/open_ecu if navigation helps; tell the user when you changed the view.
- Never expose or ask for API keys, and never send secrets.
"#;

fn tool_definitions() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "search_ecus",
                "description": "Search the ddt4all ECU database by ECU name, group, address, project code or car model name.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Free-text search, e.g. 'Clio IV' or 'UCH'"},
                        "limit": {"type": "integer", "minimum": 1, "maximum": 40}
                    },
                    "required": ["query"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_screens",
                "description": "List the screens/categories of the currently opened ECU.",
                "parameters": {"type": "object", "properties": {}}
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_current_values",
                "description": "Return the latest values from the currently displayed screen.",
                "parameters": {"type": "object", "properties": {}}
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_dtcs",
                "description": "Read the fault memory of the currently opened ECU (read-only).",
                "parameters": {"type": "object", "properties": {}}
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_request",
                "description": "Read one diagnostic request from the opened ECU by name and decode its values.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "request": {"type": "string", "description": "Exact request name, e.g. 'Trame 16'"},
                        "inputs": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Optional input values by data-item name"}
                    },
                    "required": ["request"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "open_screen",
                "description": "Open a screen by name in the current ECU.",
                "parameters": {
                    "type": "object",
                    "properties": {"name": {"type": "string"}},
                    "required": ["name"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "open_ecu",
                "description": "Open an ECU by href/name from search_ecus.",
                "parameters": {
                    "type": "object",
                    "properties": {"href": {"type": "string"}},
                    "required": ["href"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "get_ecu_info",
                "description": "Return metadata, identification records and counts for the opened ECU.",
                "parameters": {"type": "object", "properties": {}}
            }
        }
    ])
}

fn execute_tool(
    name: &str,
    args: &Value,
    context: &AgentContext,
    commands: &Sender<Command>,
    actions: &Sender<AgentAction>,
) -> Value {
    match name {
        "search_ecus" => search_ecus(context, args),
        "list_screens" => json!({
            "screens": context.screens.iter().map(|(cat, name)| json!({"category": cat, "name": name})).collect::<Vec<_>>()
        }),
        "read_current_values" => json!({
            "screen": context.screen,
            "values": context.values.iter().map(|v| json!({
                "name": v.name, "request": v.request, "value": v.value, "unit": v.unit
            })).collect::<Vec<_>>()
        }),
        "read_dtcs" => read_dtcs(context, commands),
        "read_request" => read_request(context, args, commands),
        "open_screen" => {
            let Some(name) = args["name"].as_str() else {
                return json!({"error": "missing screen name"});
            };
            let Some(canonical) = context
                .screens
                .iter()
                .find(|(_, s)| s.eq_ignore_ascii_case(name))
                .map(|(_, s)| s.clone())
            else {
                return json!({"error": format!("screen not found: {name}")});
            };
            match actions.send(AgentAction::OpenScreen(canonical.clone())) {
                Ok(_) => json!({"ok": true, "requested": canonical}),
                Err(_) => json!({"error": "UI action channel closed"}),
            }
        }
        "open_ecu" => {
            let Some(href) = args["href"].as_str() else {
                return json!({"error": "missing href"});
            };
            match actions.send(AgentAction::OpenEcu(href.to_string())) {
                Ok(_) => {
                    json!({"ok": true, "requested": href, "note": "The ECU is loading; its context will be available in the next turn."})
                }
                Err(_) => json!({"error": "UI action channel closed"}),
            }
        }
        "get_ecu_info" => {
            let Some(ecu) = &context.ecu else {
                return json!({"error": "no ECU open"});
            };
            json!({
                "ecuname": ecu.ecuname,
                "href": ecu.href,
                "protocol": ecu.protocol.to_string(),
                "address": context.address,
                "group": context.group,
                "send_id": ecu.send_id,
                "recv_id": ecu.recv_id,
                "baudrate": ecu.baudrate,
                "requests": ecu.requests.len(),
                "data": ecu.data.len(),
                "screens": context.screens.len(),
                "autoidents": ecu.autoidents.iter().map(|a| json!({
                    "diagversion": a.diagversion, "supplier": a.supplier, "soft": a.soft, "version": a.version
                })).collect::<Vec<_>>(),
            })
        }
        other => json!({"error": format!("unknown tool {other}")}),
    }
}

fn search_ecus(context: &AgentContext, args: &Value) -> Value {
    let query = args["query"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let limit = args["limit"].as_u64().unwrap_or(20).min(40) as usize;
    let vehicle = context
        .vehicle
        .as_deref()
        .and_then(|v| context.db.vehicle(v));
    let mut rows = Vec::new();
    for target in context.db.vehicle_targets(vehicle) {
        let mut hay = format!(
            "{} {} {} {}",
            target.href, target.ecuname, target.group, target.address
        )
        .to_ascii_lowercase();
        for project in &target.projects {
            hay.push(' ');
            hay.push_str(&project.to_ascii_lowercase());
            for v in &context.db.vehicles {
                if v.code.eq_ignore_ascii_case(project) {
                    hay.push(' ');
                    hay.push_str(&v.key.to_ascii_lowercase());
                }
            }
        }
        if !query.is_empty() && !hay.contains(&query) {
            continue;
        }
        rows.push(json!({
            "href": target.href,
            "ecuname": target.ecuname,
            "group": target.group,
            "protocol": target.protocol.to_string(),
            "address": target.address,
            "projects": target.projects,
        }));
        if rows.len() >= limit {
            break;
        }
    }
    json!({"count": rows.len(), "results": rows})
}

fn read_dtcs(context: &AgentContext, commands: &Sender<Command>) -> Value {
    if !context.connected {
        return json!({"error": "not connected to an ECU"});
    }
    let (tx, rx) = crossbeam_channel::bounded(1);
    if commands.send(Command::ReadDtcSync { reply: tx }).is_err() {
        return json!({"error": "diagnostic worker unavailable"});
    }
    match rx.recv_timeout(Duration::from_secs(20)) {
        Ok(Ok(report)) => dtc_json(&report),
        Ok(Err(e)) => json!({"error": e}),
        Err(_) => json!({"error": "timed out reading DTCs"}),
    }
}

fn dtc_json(report: &DtcReport) -> Value {
    json!({
        "count": report.records.len(),
        "raw": ddtrust_core::hexutil::to_hex(&report.raw),
        "records": report.records.iter().map(|r| json!({
            "index": r.index,
            "code": r.code_text(),
            "name": r.name,
            "raw": ddtrust_core::hexutil::to_hex(&r.raw),
            "fields": r.fields.iter().map(|f| json!({
                "name": f.name, "value": f.value, "raw": f.raw_hex, "unit": f.unit
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

fn read_request(context: &AgentContext, args: &Value, commands: &Sender<Command>) -> Value {
    let Some(ecu) = &context.ecu else {
        return json!({"error": "no ECU open"});
    };
    if !context.connected {
        return json!({"error": "not connected"});
    }
    let Some(request_name) = args["request"].as_str() else {
        return json!({"error": "missing request name"});
    };
    let Some(req) = ecu.request(request_name) else {
        let similar: Vec<&String> = ecu
            .requests
            .keys()
            .filter(|k| {
                k.to_ascii_lowercase()
                    .contains(&request_name.to_ascii_lowercase())
            })
            .take(8)
            .collect();
        return json!({"error": format!("request not found: {request_name}"), "similar": similar});
    };
    let mut inputs = Vec::new();
    if let Some(obj) = args["inputs"].as_object() {
        for (k, v) in obj {
            inputs.push((k.clone(), v.as_str().unwrap_or_default().to_string()));
        }
    }
    let stream = match req.build_stream(&inputs, &ecu.data, ecu.endian) {
        Ok(s) => s,
        Err(e) => return json!({"error": e.to_string()}),
    };
    if let Err(e) = Safety::default().check(&stream) {
        return json!({"error": e.to_string(), "note": "The agent is intentionally read-only."});
    }
    let (tx, rx) = crossbeam_channel::bounded(1);
    let tag = req.name.clone();
    if commands
        .send(Command::PollSync {
            items: vec![PollItem {
                tag: tag.clone(),
                payload: stream.clone(),
            }],
            reply: tx,
        })
        .is_err()
    {
        return json!({"error": "diagnostic worker unavailable"});
    }
    match rx.recv_timeout(Duration::from_secs(15)) {
        Ok(results) => {
            let Some(first) = results.into_iter().next() else {
                return json!({"error": "empty diagnostic response"});
            };
            match first.result {
                Ok(raw) => {
                    let values = req
                        .decode(&raw, &ecu.data, ecu.endian)
                        .into_iter()
                        .map(|v| {
                            json!({
                                "name": v.name, "value": v.display, "raw": v.raw_hex, "unit": v.unit
                            })
                        })
                        .collect::<Vec<_>>();
                    json!({
                        "request": req.name,
                        "sent": ddtrust_core::hexutil::to_hex_spaced(&stream),
                        "raw": ddtrust_core::hexutil::to_hex(&raw),
                        "values": values,
                    })
                }
                Err(e) => json!({"error": e}),
            }
        }
        Err(_) => json!({"error": "timed out reading request"}),
    }
}

fn context_json(context: &AgentContext) -> Value {
    let ecu = context.ecu.as_ref().map(|e| {
        json!({
            "name": e.ecuname,
            "href": e.href,
            "protocol": e.protocol.to_string(),
            "address": context.address,
            "group": context.group,
            "send_id": e.send_id,
            "recv_id": e.recv_id,
            "baud": e.baudrate,
            "ident": e.autoidents.first().map(|a| json!({
                "diagversion": a.diagversion, "supplier": a.supplier, "soft": a.soft, "version": a.version
            })),
        })
    });
    let values: Vec<Value> = context
        .values
        .iter()
        .take(100)
        .map(|v| json!({"name": v.name, "request": v.request, "value": v.value, "unit": v.unit}))
        .collect();
    let screens: Vec<Value> = context
        .screens
        .iter()
        .take(250)
        .map(|(cat, s)| json!({"category": cat, "name": s}))
        .collect();
    json!({
        "vehicle": context.vehicle,
        "connected": context.connected,
        "ecu": ecu,
        "screen": context.screen,
        "screens": screens,
        "values": values,
        "dtc": context.dtc.as_ref().map(dtc_json),
        "database": {
            "ecus": context.db.targets.len(),
            "vehicles": context.db.vehicles.len(),
        },
        "note": "Values are the latest snapshot from the GUI; use tools for fresh reads.",
    })
}

fn tool_summary(name: &str, result: &Value) -> String {
    match name {
        "search_ecus" => format!(
            "Found {} ECU definition(s)",
            result["results"].as_array().map(|a| a.len()).unwrap_or(0)
        ),
        "read_dtcs" => format!(
            "Read {} DTC record(s)",
            result["records"].as_array().map(|a| a.len()).unwrap_or(0)
        ),
        "read_request" => format!("Read request {}", result["request"].as_str().unwrap_or("?")),
        "read_current_values" => format!(
            "Returned {} value(s)",
            result["values"].as_array().map(|a| a.len()).unwrap_or(0)
        ),
        "open_screen" => format!(
            "Requested screen {}",
            result["requested"].as_str().unwrap_or("?")
        ),
        "open_ecu" => format!(
            "Requested ECU {}",
            result["requested"].as_str().unwrap_or("?")
        ),
        other => other.to_string(),
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect::<String>() + "…"
    }
}

// Silence unused import warnings in case the API surface changes.
#[allow(dead_code)]
fn _protocol(_: Protocol) {}
