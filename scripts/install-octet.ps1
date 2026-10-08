[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [string] $Version = '0.9.0',
    [string] $InstallDirectory = (Join-Path $env:LOCALAPPDATA 'Programs\octet\bin')
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$target = 'x86_64-pc-windows-msvc'
$base = "https://github.com/skaft-software/octet/releases/download/v$Version"
$archiveName = "octet-$Version-$target.zip"
$temp = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $temp | Out-Null
try {
    $archive = Join-Path $temp $archiveName
    $sums = Join-Path $temp 'OCTET_SHA256SUMS'
    Invoke-WebRequest -Uri "$base/$archiveName" -OutFile $archive
    Invoke-WebRequest -Uri "$base/OCTET_SHA256SUMS" -OutFile $sums
    $line = Get-Content -LiteralPath $sums | Where-Object { $_ -match "  \./$([regex]::Escape($archiveName))$" }
    if (@($line).Count -ne 1) { throw 'Release checksum manifest does not contain exactly one Windows archive entry.' }
    $expected = ($line -split '\s+')[0]
    $actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $expected) { throw 'Windows archive SHA-256 verification failed.' }
    $unpacked = Join-Path $temp 'unpacked'
    Expand-Archive -LiteralPath $archive -DestinationPath $unpacked
    $root = Join-Path $unpacked "octet-$Version-$target"
    foreach ($name in @('octet.exe', 'octet-host.exe')) {
        if (-not (Test-Path -LiteralPath (Join-Path $root $name) -PathType Leaf)) { throw "Release archive is missing $name." }
    }
    New-Item -ItemType Directory -Force -Path $InstallDirectory | Out-Null
    Copy-Item -LiteralPath (Join-Path $root 'octet.exe'), (Join-Path $root 'octet-host.exe') -Destination $InstallDirectory -Force
    Write-Output "Installed octet $Version to $InstallDirectory. Add this directory to PATH if needed."
    Write-Warning 'SHA-256 detects transfer corruption; this script does not independently verify the Sigstore signature.'
}
finally {
    Remove-Item -LiteralPath $temp -Recurse -Force -ErrorAction SilentlyContinue
}
