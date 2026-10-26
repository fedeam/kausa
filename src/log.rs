//! kausa's log: one JSON object per line on standard output, written through a serializer so that
//! no value taken from the cluster can break a line or forge a field.

use std::io::{self, Write};

use k8s_openapi::jiff::Timestamp;
use serde_json::Value;

/// Writes one log line: `ts`, `level` and `msg` first, then the fields of `fields` (an object) in
/// alphabetical order. A field named `ts`, `level` or `msg` is dropped, so it cannot override them.
/// The line is written with a single write, and the error is returned: `kausa run` stops when it
/// cannot write its log (exit code 2).
pub fn line(level: &str, msg: &str, fields: Value) -> io::Result<()> {
    let mut buffer = String::from("{");
    push(
        &mut buffer,
        "ts",
        &Value::from(Timestamp::now().to_string()),
    );
    buffer.push(',');
    push(&mut buffer, "level", &Value::from(level));
    buffer.push(',');
    push(&mut buffer, "msg", &Value::from(msg));
    if let Value::Object(fields) = fields {
        for (key, value) in &fields {
            if matches!(key.as_str(), "ts" | "level" | "msg") {
                continue;
            }
            buffer.push(',');
            push(&mut buffer, key, value);
        }
    }
    buffer.push_str("}\n");
    let mut out = io::stdout().lock();
    out.write_all(buffer.as_bytes())?;
    out.flush()
}

fn push(buffer: &mut String, key: &str, value: &Value) {
    // Serializing a string key or a `Value` cannot fail.
    buffer.push_str(&Value::from(key).to_string());
    buffer.push(':');
    buffer.push_str(&value.to_string());
}

pub fn info(msg: &str, fields: Value) -> io::Result<()> {
    line("info", msg, fields)
}

pub fn warn(msg: &str, fields: Value) -> io::Result<()> {
    line("warn", msg, fields)
}

pub fn error(msg: &str, fields: Value) -> io::Result<()> {
    line("error", msg, fields)
}
