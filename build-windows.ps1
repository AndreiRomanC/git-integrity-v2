$ErrorActionPreference = "Stop"

$ProjectRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$Manifest = Join-Path $ProjectRoot "src-tauri\Cargo.toml"
$BuildDir = Join-Path $ProjectRoot ".build-windows"
$OutputDir = Join-Path $ProjectRoot "dist\windows"
$OutputExe = Join-Path $OutputDir "git-integrity.exe"

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "Rust/Cargo is required only on the build machine. The final EXE needs neither Rust nor Node.js."
}

New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
$env:CARGO_TARGET_DIR = $BuildDir
$env:CARGO_INCREMENTAL = "0"

try {
    cargo build --release --manifest-path $Manifest
    Copy-Item (Join-Path $BuildDir "release\git-integrity.exe") $OutputExe -Force

    $Size = (Get-Item $OutputExe).Length
    $Limit = 30MB
    if ($Size -gt $Limit) {
        Remove-Item $OutputExe -Force
        throw "Windows executable exceeds 30 MB: $([math]::Round($Size / 1MB, 2)) MB"
    }

    Write-Host "Windows executable created: $OutputExe"
    Write-Host "Size: $([math]::Round($Size / 1MB, 2)) MB"
}
finally {
    cargo clean --manifest-path $Manifest --target-dir $BuildDir
}
