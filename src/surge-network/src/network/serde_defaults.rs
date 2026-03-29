// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared serde default-value functions for `#[serde(default = "...")]`.

pub(crate) fn default_true() -> bool {
    true
}

pub(crate) fn default_one() -> f64 {
    1.0
}

pub(crate) fn default_empty_string() -> String {
    String::new()
}
