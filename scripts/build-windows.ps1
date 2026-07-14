param(
    [ValidateSet("Debug", "Release")]
    [string]$Configuration = "Release",
    [switch]$CheckOnly
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$Target = "x86_64-pc-windows-msvc"
$Manifest = Join-Path $Root "apps/windows/Cargo.toml"

rustup target add $Target
cargo fmt --all -- --check
cargo check --manifest-path $Manifest --target $Target

$Profile = if ($Configuration -eq "Release") { "release" } else { "debug" }
$ProfileArgs = if ($Configuration -eq "Release") { @("--release") } else { @() }
cargo build --manifest-path $Manifest --target $Target @ProfileArgs

if ($CheckOnly) {
    $Executable = Join-Path $Root "target/$Target/$Profile/desklink-windows.exe"
    if (-not (Test-Path $Executable)) { throw "Windows executable was not produced" }
    $Header = [System.IO.File]::ReadAllBytes($Executable)[0..1]
    if ($Header[0] -ne 0x4d -or $Header[1] -ne 0x5a) {
        throw "desklink-windows.exe is not a PE image"
    }
}
