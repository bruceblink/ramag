//! MongoDB 值的单元格显示转换。

use serde_json::Value;

#[derive(Debug, Clone)]
pub struct Cell {
    pub text: String,
    pub kind: &'static str,
}

/// 嵌套对象和数组显示摘要，标量显示实际值。
pub(super) fn cell_for_value(v: &Value) -> Cell {
    match v {
        Value::Object(map) => extjson_cell(map).unwrap_or_else(|| Cell {
            text: format!("{{{} 字段}}", map.len()),
            kind: "object",
        }),
        Value::Array(arr) => Cell {
            text: format!("[{} 项]", arr.len()),
            kind: "array",
        },
        _ => scalar_to_cell(v).unwrap_or_else(|| Cell {
            text: String::new(),
            kind: "null",
        }),
    }
}

/// 识别 Extended JSON v2 的 canonical 与 relaxed 包装。
pub(super) fn extjson_cell(map: &serde_json::Map<String, Value>) -> Option<Cell> {
    if let Some(v) = map.get("$oid").and_then(|x| x.as_str()) {
        return Some(Cell {
            text: v.to_string(),
            kind: "oid",
        });
    }
    if let Some(v) = map.get("$numberDecimal").and_then(|x| x.as_str()) {
        return Some(Cell {
            text: v.to_string(),
            kind: "decimal",
        });
    }
    if let Some(d) = map.get("$date") {
        let text = match d {
            Value::String(s) => s.clone(),
            Value::Object(inner) => inner
                .get("$numberLong")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| d.to_string()),
            _ => d.to_string(),
        };
        return Some(Cell { text, kind: "date" });
    }
    if let Some(b) = map.get("$binary").and_then(|x| x.as_object())
        && let Some(b64) = b.get("base64").and_then(|x| x.as_str())
    {
        let sub = b.get("subType").and_then(|x| x.as_str()).unwrap_or("00");
        return Some(Cell {
            text: format!("[binary {} b64chars, subType={sub}]", b64.len()),
            kind: "binary",
        });
    }
    if let Some(v) = map.get("$numberLong").and_then(|x| x.as_str()) {
        return Some(Cell {
            text: v.to_string(),
            kind: "long",
        });
    }
    if let Some(v) = map.get("$numberInt").and_then(|x| x.as_str()) {
        return Some(Cell {
            text: v.to_string(),
            kind: "int",
        });
    }
    if let Some(v) = map.get("$numberDouble").and_then(|x| x.as_str()) {
        return Some(Cell {
            text: v.to_string(),
            kind: "double",
        });
    }
    if let Some(re) = map.get("$regularExpression").and_then(|x| x.as_object()) {
        let pattern = re.get("pattern").and_then(|x| x.as_str()).unwrap_or("");
        let options = re.get("options").and_then(|x| x.as_str()).unwrap_or("");
        return Some(Cell {
            text: format!("/{pattern}/{options}"),
            kind: "regex",
        });
    }
    if let Some(ts) = map.get("$timestamp").and_then(|x| x.as_object()) {
        let t = ts.get("t").and_then(|x| x.as_u64()).unwrap_or(0);
        let i = ts.get("i").and_then(|x| x.as_u64()).unwrap_or(0);
        return Some(Cell {
            text: format!("Timestamp({t}, {i})"),
            kind: "ts",
        });
    }
    if map.contains_key("$minKey") {
        return Some(Cell {
            text: "MinKey".to_string(),
            kind: "minkey",
        });
    }
    if map.contains_key("$maxKey") {
        return Some(Cell {
            text: "MaxKey".to_string(),
            kind: "maxkey",
        });
    }
    if map.contains_key("$undefined") {
        return Some(Cell {
            text: "undefined".to_string(),
            kind: "undef",
        });
    }
    if let Some(code) = map.get("$code").and_then(|x| x.as_str()) {
        return Some(Cell {
            text: code.to_string(),
            kind: "code",
        });
    }
    if let Some(s) = map.get("$symbol").and_then(|x| x.as_str()) {
        return Some(Cell {
            text: s.to_string(),
            kind: "symbol",
        });
    }
    if map.contains_key("$dbPointer") {
        return Some(Cell {
            text: serde_json::to_string(&map["$dbPointer"]).unwrap_or_default(),
            kind: "dbptr",
        });
    }
    None
}

pub(super) fn scalar_to_cell(v: &Value) -> Option<Cell> {
    match v {
        Value::Null => Some(Cell {
            text: String::new(),
            kind: "null",
        }),
        Value::Bool(b) => Some(Cell {
            text: b.to_string(),
            kind: "bool",
        }),
        Value::Number(n) => {
            let kind = if n.is_i64() || n.is_u64() {
                "int"
            } else {
                "double"
            };
            Some(Cell {
                text: n.to_string(),
                kind,
            })
        }
        Value::String(s) => Some(Cell {
            text: s.clone(),
            kind: "text",
        }),
        _ => None,
    }
}
