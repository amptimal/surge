// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../surge-opf/copt_nlp_shim.cpp");
    println!("cargo:rerun-if-changed=../../scripts/build-copt-nlp-shim.sh");
    println!("cargo:rerun-if-changed=../../scripts/build-copt-nlp-shim.ps1");
    println!("cargo:rerun-if-env-changed=COPT_HOME");
    println!("cargo:rerun-if-env-changed=SURGE_PY_REQUIRE_COPT_NLP_SHIM");
    println!("cargo:rerun-if-env-changed=SURGE_COPT_NLP_SHIM_OUT");
    println!("cargo:rerun-if-env-changed=CXX");

    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let package_dir = manifest_dir.join("python").join("surge");
    let shim_paths = packaged_shim_paths(&package_dir);
    let require_shim = env_flag("SURGE_PY_REQUIRE_COPT_NLP_SHIM");

    match maybe_build_packaged_copt_nlp_shim(&manifest_dir, &package_dir) {
        Ok(Some(path)) => {
            println!(
                "cargo:warning=Bundled COPT NLP shim for surge-py wheel at {}",
                path.display()
            );
        }
        Ok(None) => {
            remove_stale_shims(&shim_paths);
            if require_shim {
                panic!(
                    "SURGE_PY_REQUIRE_COPT_NLP_SHIM=1 but COPT_HOME is not configured or COPT 8.x headers/libs are unavailable"
                );
            }
            println!(
                "cargo:warning=Skipping bundled COPT NLP shim; set COPT_HOME to a COPT 8.x install to bundle it into surge-py wheels"
            );
        }
        Err(err) => {
            remove_stale_shims(&shim_paths);
            if require_shim {
                panic!("failed to build bundled COPT NLP shim: {err}");
            }
            println!("cargo:warning=Failed to build bundled COPT NLP shim: {err}");
        }
    }
}

fn env_flag(name: &str) -> bool {
    match env::var(name) {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

fn maybe_build_packaged_copt_nlp_shim(
    manifest_dir: &Path,
    package_dir: &Path,
) -> Result<Option<PathBuf>, String> {
    let copt_home = match env::var("COPT_HOME") {
        Ok(home) if !home.trim().is_empty() => PathBuf::from(home),
        _ => return Ok(None),
    };

    let headers_dir = copt_home.join("include").join("coptcpp_inc");
    let lib_dir = copt_home.join("lib");
    if !headers_dir.is_dir() || !lib_dir.is_dir() {
        return Ok(None);
    }

    fs::create_dir_all(package_dir).map_err(|e| {
        format!(
            "could not create Python package directory {}: {e}",
            package_dir.display()
        )
    })?;

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let output_path = package_dir.join(shim_file_name(&target_os)?);
    let repo_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| {
            format!(
                "could not resolve repo root from {}",
                manifest_dir.display()
            )
        })?;

    let status = if target_os == "windows" {
        Command::new("powershell")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(repo_root.join("scripts").join("build-copt-nlp-shim.ps1"))
            .env("COPT_HOME", &copt_home)
            .env("SURGE_COPT_NLP_SHIM_OUT", &output_path)
            .status()
            .map_err(|e| format!("failed to invoke build-copt-nlp-shim.ps1: {e}"))?
    } else {
        Command::new("bash")
            .arg(repo_root.join("scripts").join("build-copt-nlp-shim.sh"))
            .env("COPT_HOME", &copt_home)
            .env("SURGE_COPT_NLP_SHIM_OUT", &output_path)
            .status()
            .map_err(|e| format!("failed to invoke build-copt-nlp-shim.sh: {e}"))?
    };

    if !status.success() {
        return Err(format!(
            "shim build script exited with status {status} for {}",
            output_path.display()
        ));
    }
    if !output_path.is_file() {
        return Err(format!(
            "shim build completed but {} was not created",
            output_path.display()
        ));
    }

    Ok(Some(output_path))
}

fn shim_file_name(target_os: &str) -> Result<&'static str, String> {
    match target_os {
        "linux" => Ok("libsurge_copt_nlp.so"),
        "macos" => Ok("libsurge_copt_nlp.dylib"),
        "windows" => Ok("surge_copt_nlp.dll"),
        other => Err(format!(
            "unsupported target OS for bundled COPT NLP shim: {other}"
        )),
    }
}

fn packaged_shim_paths(package_dir: &Path) -> [PathBuf; 3] {
    [
        package_dir.join("libsurge_copt_nlp.so"),
        package_dir.join("libsurge_copt_nlp.dylib"),
        package_dir.join("surge_copt_nlp.dll"),
    ]
}

fn remove_stale_shims(paths: &[PathBuf; 3]) {
    for path in paths {
        if path.is_file() {
            let _ = fs::remove_file(path);
        }
    }
}
