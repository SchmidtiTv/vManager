# Builds a release and installs it to C:\dev\tool\vmanager: both executables.

$ErrorActionPreference = 'Stop'

$Destination = 'C:\dev\tool\vmanager'

cargo build --release --manifest-path "$PSScriptRoot\Cargo.toml"
if ($LASTEXITCODE -ne 0) {
    throw "cargo build --release failed with exit code $LASTEXITCODE; nothing was copied"
}

New-Item -ItemType Directory -Force -Path $Destination | Out-Null

foreach ($exe in 'vmanager.exe', 'vman.exe') {
    Copy-Item -Path "$PSScriptRoot\target\release\$exe" -Destination $Destination -Force
}

Write-Host "Installed vmanager.exe and vman.exe to $Destination"
