//! The JSON contract of the CLI's output (story "Coverage floor and property
//! tests for the safety invariants", DDC-005): what a command prints is valid
//! against its committed schema in `docs/` and reads back into its type
//! unchanged. The drift tests keep each schema equal to its type; this keeps
//! the output equal to both, so a field the type skips, renames or defaults
//! fails here, not in the app.

use std::fs;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// `json` checked against `docs/<schema>.schema.json` and read back through
/// `T`; the value read.
pub fn holds<T: Serialize + DeserializeOwned>(schema: &str, json: &[u8]) -> T {
    let text = String::from_utf8_lossy(json);
    let value: Value = serde_json::from_slice(json).unwrap_or_else(|e| panic!("{schema}: {e}: {text}"));
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../docs/{schema}.schema.json"));
    let doc: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let validator = jsonschema::validator_for(&doc).unwrap();
    let errors: Vec<String> =
        validator.iter_errors(&value).map(|e| format!("{} at {}", e, e.instance_path())).collect();
    assert!(errors.is_empty(), "not valid against docs/{schema}.schema.json:\n{}\n{text}", errors.join("\n"));
    let typed: T = serde_json::from_value(value.clone()).unwrap_or_else(|e| panic!("{schema}: {e}: {text}"));
    let again = serde_json::to_value(&typed).unwrap();
    if let Some(at) = first_difference(&value, &again, String::new()) {
        panic!("{schema}: read back and written again, it differs at {at}");
    }
    typed
}

/// The JSON pointer of the first place `a` and `b` differ, with both values.
fn first_difference(a: &Value, b: &Value, at: String) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let keys: std::collections::BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            keys.into_iter().find_map(|k| match (x.get(k), y.get(k)) {
                (Some(p), Some(q)) => first_difference(p, q, format!("{at}/{k}")),
                (p, q) => Some(format!("{at}/{k}: printed {p:?}, written again {q:?}")),
            })
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            x.iter().zip(y).enumerate().find_map(|(i, (p, q))| first_difference(p, q, format!("{at}/{i}")))
        }
        _ => (a != b).then(|| format!("{at}: printed {a}, written again {b}")),
    }
}
