// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Emit the JSON schema for [`surge_dispatch::DispatchRequest`].
//!
//! Used by `scripts/codegen_dispatch_request.py` to regenerate
//! `src/surge-py/python/surge/_generated/dispatch_request.py`. Prints
//! the schema to stdout; run via:
//!
//! ```bash
//! cargo run -q -p surge-dispatch --bin emit-schema
//! ```

use schemars::schema_for;
use surge_dispatch::DispatchRequest;

fn main() {
    let schema = schema_for!(DispatchRequest);
    let json = serde_json::to_string_pretty(&schema)
        .expect("serializing JSON schema for DispatchRequest must succeed");
    println!("{json}");
}
