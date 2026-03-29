# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
#
# Build the COPT NLP shim as a standalone DLL on Windows (MSVC).
#
# Requires COPT 8.x headers at $env:COPT_HOME\include\coptcpp_inc\.
# Produces surge_copt_nlp.dll and writes it to
# $env:SURGE_COPT_NLP_SHIM_OUT (or $env:COPT_HOME\lib\ by default).
#
# Usage (from repo root, in a Developer Command Prompt or with cl.exe on PATH):
#   .\scripts\build-copt-nlp-shim.ps1
#   $env:COPT_HOME = "C:\copt80"; .\scripts\build-copt-nlp-shim.ps1
param(
    [string]$CoptHome = $env:COPT_HOME
)

$ErrorActionPreference = "Stop"

if (-not $CoptHome) {
    $CoptHome = "C:\copt80"
}

$Root    = Split-Path -Parent $PSScriptRoot
$Src     = Join-Path $Root "src\surge-opf\copt_nlp_shim.cpp"
$Headers = Join-Path $CoptHome "include\coptcpp_inc"
$LibDir  = Join-Path $CoptHome "lib"
$OutDll  = if ($env:SURGE_COPT_NLP_SHIM_OUT) { $env:SURGE_COPT_NLP_SHIM_OUT } else { Join-Path $LibDir "surge_copt_nlp.dll" }

if (-not (Test-Path $Src)) {
    Write-Error "Shim source not found at $Src"
}
if (-not (Test-Path $Headers)) {
    Write-Error "COPT C++ headers not found at $Headers. Set COPT_HOME to your COPT 8.x installation."
}
if (-not (Test-Path $LibDir)) {
    Write-Error "COPT lib directory not found at $LibDir"
}

Write-Host "Compiling surge_copt_nlp.dll (Windows/MSVC)..."
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $OutDll) | Out-Null

# Ensure cl.exe is available -- if not already on PATH, initialise the MSVC
# developer environment via vcvarsall.bat (found through vswhere).
if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue)) {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path $vswhere)) {
        Write-Error "cl.exe is not on PATH and vswhere.exe was not found -- install Visual Studio Build Tools"
    }
    $vsInstall = & $vswhere -latest -property installationPath
    $vcvarsall = Join-Path $vsInstall "VC\Auxiliary\Build\vcvarsall.bat"
    if (-not (Test-Path $vcvarsall)) {
        Write-Error "vcvarsall.bat not found at $vcvarsall"
    }
    # Run vcvarsall in a cmd child and import the environment variables it sets.
    $arch = if ([Environment]::Is64BitOperatingSystem) { "amd64" } else { "x86" }
    $bat = Join-Path $env:TEMP "surge_vcvars_$PID.bat"
    Set-Content -Path $bat -Value (@'
@call "{0}" {1} >nul 2>&1
@if errorlevel 1 exit /b 1
@set
'@ -f $vcvarsall, $arch)
    $envBlock = cmd /c $bat 2>&1
    Remove-Item $bat -ErrorAction SilentlyContinue
    foreach ($line in $envBlock) {
        if ($line -match '^([^=]+)=(.*)$') {
            [Environment]::SetEnvironmentVariable($Matches[1], $Matches[2], 'Process')
        }
    }
    if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue)) {
        Write-Error "Failed to locate cl.exe after running vcvarsall.bat"
    }
    Write-Host "Initialized MSVC environment via vcvarsall.bat"
}

cl.exe /LD /O2 /std:c++17 /EHsc `
    /I"$CoptHome\include" /I"$Headers" `
    $Src `
    /link /LIBPATH:"$LibDir" copt_cpp.lib `
    /OUT:"$OutDll"

if ($LASTEXITCODE -ne 0) {
    Write-Error "Compilation failed (exit code $LASTEXITCODE)"
}

Write-Host "Installed $OutDll"

# Verify the symbol is exported.
$dumpbin = Get-Command dumpbin.exe -ErrorAction SilentlyContinue
if ($dumpbin) {
    $exports = & dumpbin.exe /EXPORTS $OutDll 2>&1 | Select-String "copt_nlp_solve"
    if ($exports) {
        Write-Host "Symbol copt_nlp_solve: OK"
    } else {
        Write-Warning "Symbol copt_nlp_solve not found in exports!"
    }
}
