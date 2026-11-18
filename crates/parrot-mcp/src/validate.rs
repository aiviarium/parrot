//! Checks tool arguments against a function's input schema before they leave the server, for the
//! schema subset functions may use (spec 27.1). A bad call then fails at once with
//! `invalid_input` and the path of the first problem, without a trip to the runtime.

use serde_json::Value;

/// The first way `value` breaks `schema`, as `<path>: <problem>`.
pub fn check(schema: &Value, value: &Value) -> Result<(), String> {
    at(schema, value, "input")
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn is_type(value: &Value, ty: &str) -> bool {
    match ty {
        // A float with no fraction counts as an integer, as JSON Schema has it.
        "integer" => value.as_f64().is_some_and(|f| f.fract() == 0.0),
        "number" => value.is_number(),
        other => type_name(value) == other,
    }
}

fn at(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    let Some(schema) = schema.as_object() else {
        return Ok(()); // `true` or a shape this server does not read
    };
    if let Some(ty) = schema.get("type") {
        let types: Vec<&str> = match ty {
            Value::String(t) => vec![t.as_str()],
            Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        if !types.is_empty() && !types.iter().any(|t| is_type(value, t)) {
            return Err(format!(
                "{path}: expected {}, got {}",
                types.join(" or "),
                type_name(value)
            ));
        }
    }
    if let Some(c) = schema.get("const")
        && c != value
    {
        return Err(format!("{path}: must be {c}"));
    }
    if let Some(Value::Array(options)) = schema.get("enum")
        && !options.contains(value)
    {
        let list: Vec<String> = options.iter().map(Value::to_string).collect();
        return Err(format!("{path}: must be one of {}", list.join(", ")));
    }
    if let Some(n) = value.as_f64() {
        if let Some(min) = schema.get("minimum").and_then(Value::as_f64)
            && n < min
        {
            return Err(format!("{path}: must be at least {min}"));
        }
        if let Some(max) = schema.get("maximum").and_then(Value::as_f64)
            && n > max
        {
            return Err(format!("{path}: must be at most {max}"));
        }
    }
    if let (Some(s), Some(pattern)) = (value.as_str(), schema.get("pattern").and_then(Value::as_str))
        && let Ok(re) = regex::Regex::new(pattern)
        && !re.is_match(s)
    {
        return Err(format!("{path}: does not match {pattern}"));
    }
    if let (Some(items), Some(list)) = (schema.get("items"), value.as_array()) {
        for (i, item) in list.iter().enumerate() {
            at(items, item, &format!("{path}[{i}]"))?;
        }
    }
    if let Some(obj) = value.as_object() {
        if let Some(Value::Array(required)) = schema.get("required") {
            for name in required.iter().filter_map(Value::as_str) {
                if !obj.contains_key(name) {
                    return Err(format!("{path}.{name}: is required"));
                }
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        for (name, v) in obj {
            match properties.and_then(|p| p.get(name)) {
                Some(s) => at(s, v, &format!("{path}.{name}"))?,
                None if schema.get("additionalProperties") == Some(&Value::Bool(false)) => {
                    return Err(format!("{path}.{name}: is not a known field"));
                }
                None => {}
            }
        }
    }
    if let Some(Value::Array(options)) = schema.get("oneOf") {
        one_of(options, value, path)?;
    }
    Ok(())
}

/// `oneOf` of objects told apart by a `const` field: the option whose discriminator matches is
/// the one checked, so the error names that option's problem.
fn one_of(options: &[Value], value: &Value, path: &str) -> Result<(), String> {
    let chosen = options.iter().find(|o| {
        o["properties"].as_object().is_some_and(|props| {
            props
                .iter()
                .filter_map(|(k, p)| p.get("const").map(|c| (k, c)))
                .any(|(k, c)| value.get(k) == Some(c))
        })
    });
    match chosen {
        Some(option) => at(option, value, path),
        None if options.iter().any(|o| at(o, value, path).is_ok()) => Ok(()),
        None => Err(format!("{path}: matches none of the allowed shapes")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "required": ["text"],
            "properties": {
                "text": { "type": "string", "pattern": "^[a-z ]+$" },
                "count": { "type": "integer", "minimum": 1, "maximum": 10 },
                "mode": { "enum": ["fast", "slow"] },
                "tags": { "type": "array", "items": { "type": "string" } },
                "target": { "oneOf": [
                    { "type": "object", "required": ["kind", "path"], "properties": { "kind": { "const": "dir" }, "path": { "type": "string" } } },
                    { "type": "object", "required": ["kind", "url"], "properties": { "kind": { "const": "git" }, "url": { "type": "string" } } },
                ] },
            },
        })
    }

    #[test]
    fn accepts_a_good_input() {
        let input = json!({ "text": "hello there", "count": 3, "mode": "fast", "tags": ["a"],
            "target": { "kind": "git", "url": "https://git.example.test/a.git" } });
        assert_eq!(check(&schema(), &input), Ok(()));
        assert_eq!(check(&schema(), &json!({ "text": "x", "count": 2.0 })), Ok(()));
    }

    #[test]
    fn names_the_first_problem_with_its_path() {
        let s = schema();
        let bad = |v: Value| check(&s, &v).unwrap_err();
        assert_eq!(bad(json!({})), "input.text: is required");
        assert_eq!(
            bad(json!({ "text": 4 })),
            "input.text: expected string, got integer"
        );
        assert_eq!(
            bad(json!({ "text": "Hi!" })),
            "input.text: does not match ^[a-z ]+$"
        );
        assert_eq!(
            bad(json!({ "text": "a", "count": 0 })),
            "input.count: must be at least 1"
        );
        assert_eq!(
            bad(json!({ "text": "a", "count": 1.5 })),
            "input.count: expected integer, got number"
        );
        assert_eq!(
            bad(json!({ "text": "a", "mode": "odd" })),
            "input.mode: must be one of \"fast\", \"slow\""
        );
        assert_eq!(
            bad(json!({ "text": "a", "tags": ["x", 2] })),
            "input.tags[1]: expected string, got integer"
        );
        assert_eq!(
            bad(json!({ "text": "a", "target": { "kind": "dir" } })),
            "input.target.path: is required"
        );
        assert_eq!(
            bad(json!({ "text": "a", "target": { "kind": "svn" } })),
            "input.target: matches none of the allowed shapes"
        );
        assert!(
            check(
                &json!({ "type": "object", "additionalProperties": false }),
                &json!({ "x": 1 })
            )
            .is_err()
        );
    }
}
