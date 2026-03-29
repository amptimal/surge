// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
fn main() {
    println!("cargo:rerun-if-env-changed=SUITESPARSE_LIB_DIR");

    // Link SuiteSparse KLU for sparse LU factorization.
    // Linux:  sudo apt-get install libsuitesparse-dev
    // macOS:  brew install suite-sparse
    // Windows: vcpkg install suitesparse-klu:x64-windows-static

    // If SUITESPARSE_LIB_DIR is set (e.g. vcpkg on Windows, CI overrides),
    // use it directly.  Otherwise fall back to platform defaults.
    if let Ok(lib_dir) = std::env::var("SUITESPARSE_LIB_DIR") {
        println!("cargo:rustc-link-search=native={lib_dir}");
    } else {
        // On macOS, add Homebrew lib search paths so the linker can find KLU.
        // Apple Silicon (aarch64): Homebrew installs to /opt/homebrew.
        // Intel   (x86_64):        Homebrew installs to /usr/local.
        // suite-sparse is keg-only -> libraries are in opt/, not the main lib/.
        let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
        let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
        if target_os == "macos" {
            if target_arch == "aarch64" {
                println!("cargo:rustc-link-search=native=/opt/homebrew/lib");
                println!("cargo:rustc-link-search=native=/opt/homebrew/opt/suite-sparse/lib");
            } else {
                println!("cargo:rustc-link-search=native=/usr/local/lib");
                println!("cargo:rustc-link-search=native=/usr/local/opt/suite-sparse/lib");
            }
        }
    }

    // SuiteSparse 7.x names static libs with a `_static` suffix on MSVC
    // (e.g. klu_static.lib) but uses the plain name on Linux/macOS (klu.a).
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let suffix = if target_env == "msvc" { "_static" } else { "" };
    println!("cargo:rustc-link-lib=klu{suffix}");
    println!("cargo:rustc-link-lib=amd{suffix}");
    println!("cargo:rustc-link-lib=colamd{suffix}");
    println!("cargo:rustc-link-lib=btf{suffix}");
    println!("cargo:rustc-link-lib=suitesparseconfig{suffix}");
}
