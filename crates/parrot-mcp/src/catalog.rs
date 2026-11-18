//! The tools the server lists (spec 26): the session functions, pinned functions, and three tools
//! that search, describe and call every other function without listing it.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::session_functions::{self, SessionFunction};

/// The tools that reach functions the listing leaves out.
pub const SEARCH: &str = "parrot.functions.search";
pub const DESCRIBE: &str = "parrot.functions.describe";
pub const CALL: &str = "parrot.functions.call";

/// The MCP name of a function: dots become `__`.
pub fn mcp_name(function: &str) -> String {
    function.replace('.', "__")
}

/// The function name of an MCP tool name.
pub fn function_name(tool: &str) -> String {
    tool.replace("__", ".")
}

/// Checks a function name against spec 27.1.
pub fn check_name(name: &str) -> Result<(), String> {
    let segments: Vec<&str> = name.split('.').collect();
    if segments.len() > 4 {
        return Err(format!("{name}: more than four segments"));
    }
    for seg in &segments {
        let mut chars = seg.chars();
        let first_ok = chars.next().is_some_and(|c| c.is_ascii_lowercase());
        let rest_ok = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !first_ok || !rest_ok {
            return Err(format!("{name}: segment {seg:?} does not match [a-z][a-z0-9_]*"));
        }
        if seg.contains("__") {
            return Err(format!("{name}: segment {seg:?} contains __"));
        }
    }
    if mcp_name(name).len() > 64 {
        return Err(format!("{name}: the MCP name is over 64 characters"));
    }
    Ok(())
}

/// What a listed tool runs.
#[derive(Debug, Clone)]
pub enum Entry {
    Session(&'static SessionFunction),
    /// A pinned function, called through `function/call`.
    Pinned {
        function: String,
        env: Option<String>,
    },
    Search,
    Describe,
    Call,
}

#[derive(Debug, Clone)]
pub struct Tool {
    pub name: String,
    pub entry: Entry,
    pub json: Value,
}

/// What the session may use, as the runtime reported it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Access {
    pub scopes: Vec<String>,
    /// Whether the session has a task, so `parrot.task.*` apply.
    pub task: bool,
}

/// The sorted listing.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    tools: BTreeMap<String, Tool>,
}

impl Catalog {
    /// Builds the listing from the session's access and the `FunctionInfo`s of `function/list`.
    pub fn build(access: &Access, functions: &[Value]) -> Catalog {
        let mut tools = BTreeMap::new();
        for sf in session_functions::ALL {
            if sf.available(access) {
                let tool = session_tool(sf);
                tools.insert(tool.name.clone(), tool);
            }
        }
        for info in functions {
            if info.get("pinned").and_then(Value::as_bool) != Some(true) {
                continue;
            }
            let Some(name) = info.get("name").and_then(Value::as_str) else {
                continue;
            };
            if check_name(name).is_err() || session_functions::find(name).is_some() {
                continue;
            }
            let tool = pinned_tool(info);
            // A name offered by several environments is reached through `parrot.functions.call`.
            if tools.contains_key(&tool.name) {
                tools.remove(&tool.name);
                continue;
            }
            tools.insert(tool.name.clone(), tool);
        }
        for (name, entry) in [
            (SEARCH, Entry::Search),
            (DESCRIBE, Entry::Describe),
            (CALL, Entry::Call),
        ] {
            let tool = meta_tool(name, entry);
            tools.insert(tool.name.clone(), tool);
        }
        Catalog { tools }
    }

    pub fn get(&self, tool: &str) -> Option<&Tool> {
        self.tools.get(tool)
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// One page of tools after `cursor` (the last name of the previous page).
    pub fn page(&self, cursor: Option<&str>, size: usize) -> (Vec<Value>, Option<String>) {
        let size = size.max(1);
        let start = match cursor {
            Some(c) => self
                .tools
                .range::<str, _>((std::ops::Bound::Excluded(c), std::ops::Bound::Unbounded)),
            None => self.tools.range::<str, _>(..),
        };
        let page: Vec<&Tool> = start.take(size + 1).map(|(_, t)| t).collect();
        let next = (page.len() > size).then(|| page[size - 1].name.clone());
        (
            page.into_iter().take(size).map(|t| t.json.clone()).collect(),
            next,
        )
    }
}

fn tool_json(name: &str, description: &str, input: &Value, output: &Value, effects: &[&str]) -> Value {
    let read_only = effects.iter().all(|e| *e == "read");
    let destructive = effects.contains(&"destructive");
    json!({
        "name": mcp_name(name),
        "description": format!("{description}\n\nReturns: {}", ts_type(output, 0)),
        "inputSchema": input,
        "outputSchema": output,
        "annotations": { "readOnlyHint": read_only, "destructiveHint": destructive },
    })
}

fn session_tool(sf: &'static SessionFunction) -> Tool {
    Tool {
        name: mcp_name(sf.name),
        entry: Entry::Session(sf),
        json: tool_json(
            sf.name,
            sf.description,
            &sf.input_schema(),
            &(sf.output)(),
            sf.effects,
        ),
    }
}

fn pinned_tool(info: &Value) -> Tool {
    let name = info["name"].as_str().unwrap_or_default();
    let effects: Vec<&str> = info["effects"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut input = info
        .get("input")
        .cloned()
        .unwrap_or_else(|| json!({ "type": "object" }));
    if effects.iter().any(|e| *e != "read")
        && let Some(props) = input.get_mut("properties").and_then(Value::as_object_mut)
    {
        props.entry("key").or_insert_with(key_schema);
    }
    let output = info
        .get("output")
        .cloned()
        .unwrap_or_else(|| json!({ "type": "object" }));
    let description = info["description"].as_str().unwrap_or_default();
    Tool {
        name: mcp_name(name),
        entry: Entry::Pinned {
            function: name.to_string(),
            env: info.get("env").and_then(Value::as_str).map(str::to_string),
        },
        json: tool_json(name, description, &input, &output, &effects),
    }
}

/// The schema of the optional idempotency key (spec 27.1).
pub fn key_schema() -> Value {
    json!({
        "type": "string",
        "description": "Idempotency key. A repeated key returns the first call's result; reuse the key a pending_approval error returned to collect the result."
    })
}

fn meta_tool(name: &'static str, entry: Entry) -> Tool {
    let info = json!({ "type": "object", "properties": {
        "name": { "type": "string" },
        "description": { "type": "string" },
        "effects": { "type": "array", "items": { "type": "string" } },
        "env": { "type": "string" },
        "provider": { "type": "string" },
    }, "required": ["name", "description", "effects"] });
    let (description, input, output, effects): (&str, Value, Value, &[&str]) = match entry {
        Entry::Search => (
            "Search the functions this session may call by name and description. Use parrot__functions__describe for their schemas and parrot__functions__call to run them.",
            json!({ "type": "object", "properties": {
                "query": { "type": "string", "description": "Words to match against names and descriptions" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 },
            }, "required": ["query"] }),
            json!({ "type": "object", "properties": { "functions": { "type": "array", "items": info } }, "required": ["functions"] }),
            &["read"],
        ),
        Entry::Describe => (
            "Describe functions by name: input and output schemas, effects, examples, and TypeScript declarations.",
            json!({ "type": "object", "properties": {
                "names": { "type": "array", "items": { "type": "string" }, "description": "Function names, dot-separated" },
                "env": { "type": "string", "description": "The environment, when a name is offered by several" },
            }, "required": ["names"] }),
            json!({ "type": "object", "properties": {
                "functions": { "type": "array", "items": { "type": "object" } },
                "declarations": { "type": "string" },
            }, "required": ["functions", "declarations"] }),
            &["read"],
        ),
        _ => (
            "Call any function this session may call. Calls that need approval return the error pending_approval with a key; call again with that key to get the result.",
            json!({ "type": "object", "properties": {
                "function": { "type": "string", "description": "The function name, dot-separated" },
                "input": { "type": "object", "description": "The function's input", "default": {} },
                "env": { "type": "string", "description": "The environment, when the name is offered by several" },
                "key": key_schema(),
            }, "required": ["function"] }),
            json!({ "type": "object", "description": "The function's output" }),
            &["read", "write", "turn", "destructive"],
        ),
    };
    let mut json = tool_json(name, description, &input, &output, effects);
    if matches!(entry, Entry::Call) {
        json["annotations"] = json!({});
    }
    Tool {
        name: mcp_name(name),
        entry,
        json,
    }
}

/// Renders a schema as a short TypeScript type, for code bindings that don't see `outputSchema`.
pub fn ts_type(schema: &Value, depth: usize) -> String {
    if depth > 4 {
        return "unknown".into();
    }
    if let Some(c) = schema.get("const") {
        return c.to_string();
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return values
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join(" | ");
    }
    if let Some(options) = schema.get("oneOf").and_then(Value::as_array) {
        return options
            .iter()
            .map(|o| ts_type(o, depth + 1))
            .collect::<Vec<_>>()
            .join(" | ");
    }
    match schema.get("type") {
        Some(Value::String(t)) => ts_named(t, schema, depth),
        Some(Value::Array(types)) => types
            .iter()
            .filter_map(Value::as_str)
            .map(|t| ts_named(t, schema, depth))
            .collect::<Vec<_>>()
            .join(" | "),
        _ => "unknown".into(),
    }
}

fn ts_named(t: &str, schema: &Value, depth: usize) -> String {
    match t {
        "string" => "string".into(),
        "integer" | "number" => "number".into(),
        "boolean" => "boolean".into(),
        "null" => "null".into(),
        "array" => {
            let item = schema
                .get("items")
                .map(|i| ts_type(i, depth + 1))
                .unwrap_or("unknown".into());
            if item.contains(' ') {
                format!("({item})[]")
            } else {
                format!("{item}[]")
            }
        }
        "object" => {
            let empty = Map::new();
            let props = schema
                .get("properties")
                .and_then(Value::as_object)
                .unwrap_or(&empty);
            if props.is_empty() {
                return "object".into();
            }
            let required: Vec<&str> = schema
                .get("required")
                .and_then(Value::as_array)
                .map(|r| r.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let fields: Vec<String> = props
                .iter()
                .map(|(k, v)| {
                    let opt = if required.contains(&k.as_str()) { "" } else { "?" };
                    format!("{k}{opt}: {}", ts_type(v, depth + 1))
                })
                .collect();
            format!("{{ {} }}", fields.join("; "))
        }
        _ => "unknown".into(),
    }
}
