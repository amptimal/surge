// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use serde_value::Value as SerdeValue;
use surge_network::Network;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn bundle_path(name: &str) -> PathBuf {
    let cases_root = repo_root().join("examples").join("cases");
    match name {
        "case118" => cases_root.join("ieee118").join("case118.surge.json.zst"),
        _ => cases_root.join(name).join(format!("{name}.surge.json.zst")),
    }
}

pub fn load_case(name: &str) -> Network {
    let path = bundle_path(name);
    let bytes = fs::read(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    let json = zstd::stream::decode_all(Cursor::new(bytes))
        .unwrap_or_else(|err| panic!("decompress {}: {err}", path.display()));
    let document: serde_json::Value = serde_json::from_slice(&json)
        .unwrap_or_else(|err| panic!("parse {}: {err}", path.display()));
    let network_json = document.get("network").cloned().unwrap_or(document);
    let value = json_to_serde_value(network_json)
        .unwrap_or_else(|err| panic!("decode {}: {err}", path.display()));
    let mut network: Network = value
        .deserialize_into()
        .unwrap_or_else(|err| panic!("deserialize {}: {err}", path.display()));
    network.canonicalize_runtime_identities();
    network
}

fn json_to_serde_value(value: serde_json::Value) -> Result<SerdeValue, String> {
    use std::collections::BTreeMap;

    match value {
        serde_json::Value::Null => Ok(SerdeValue::Option(None)),
        serde_json::Value::Bool(value) => Ok(SerdeValue::Bool(value)),
        serde_json::Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Ok(SerdeValue::I64(value))
            } else if let Some(value) = value.as_u64() {
                Ok(SerdeValue::U64(value))
            } else if let Some(value) = value.as_f64() {
                Ok(SerdeValue::F64(value))
            } else {
                Err("unsupported JSON number representation".to_string())
            }
        }
        serde_json::Value::String(value) => Ok(SerdeValue::String(value)),
        serde_json::Value::Array(values) => Ok(SerdeValue::Seq(
            values
                .into_iter()
                .map(json_to_serde_value)
                .collect::<Result<Vec<_>, _>>()?,
        )),
        serde_json::Value::Object(mut object) => {
            if object.len() == 1
                && let Some(serde_json::Value::String(value)) = object.remove("$surge_float")
            {
                return match value.as_str() {
                    "NaN" => Ok(SerdeValue::F64(f64::NAN)),
                    "Infinity" => Ok(SerdeValue::F64(f64::INFINITY)),
                    "-Infinity" => Ok(SerdeValue::F64(f64::NEG_INFINITY)),
                    other => Err(format!("unknown special float marker {other}")),
                };
            }

            let mut map = BTreeMap::new();
            for (key, value) in object {
                map.insert(SerdeValue::String(key), json_to_serde_value(value)?);
            }
            Ok(SerdeValue::Map(map))
        }
    }
}
