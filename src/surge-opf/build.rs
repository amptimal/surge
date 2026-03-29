// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Build script for surge-opf.
//!
//! All solver backends (Gurobi, COPT, CPLEX, Ipopt, HiGHS) are loaded at
//! **runtime** via `libloading` — no link-time dependencies.
//!
//! The COPT NLP shim (`copt_nlp_shim.cpp`) is now compiled as a standalone
//! shared library via `scripts/build-copt-nlp-shim.sh` and loaded at runtime.
//! No build-time COPT dependency remains.

fn main() {
    // Deprecation notice for the old build-time shim workflow.
    if std::env::var("SURGE_ENABLE_COPT_NLP_SHIM").is_ok() {
        println!(
            "cargo:warning=SURGE_ENABLE_COPT_NLP_SHIM is deprecated and has no effect. \
             The COPT NLP shim is now loaded at runtime. \
             Build the standalone shim with: scripts/build-copt-nlp-shim.sh"
        );
    }

    println!("cargo:rerun-if-env-changed=COPT_HOME");
    println!("cargo:rerun-if-env-changed=SURGE_ENABLE_COPT_NLP_SHIM");
}
