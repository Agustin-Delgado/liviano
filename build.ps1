<#
.SYNOPSIS
    Builds, signs and packages Nea into dist\.

.DESCRIPTION
    1. Runs cargo build --release. With -Dev, it makes a debug build named
       "Nea (development)".
    2. Signs the code with signtool, if you give a certificate.
    3. Makes the portable package Nea-<version>-portable.zip (nea.exe,
       LICENSE, NOTICE).
    4. Makes the per-user installer Nea-<version>-setup.exe. This is the
       same program. Because its name contains "setup", it installs itself
       when you open it. It does not need administrator rights.
    5. With -UpdateKey, makes a signed latest.json (Ed25519) for the updater.
    6. Writes SHA256SUMS.txt with the hash of each file.

    The zip is reproducible. The entries have a fixed order and the date of
    the last commit (or SOURCE_DATE_EPOCH, if it is set).

.EXAMPLE
    .\build.ps1
.EXAMPLE
    .\build.ps1 -CertificatePath C:\certs\nea.pfx -CertificatePassword $pwd -UpdateKey keys\production.key `
        -PackageUrl https://github.com/<owner>/nea/releases/download/v0.2.0/nea.exe
#>
[CmdletBinding()]
param(
    # Development build (debug): name and title "Nea (development)".
    [switch]$Dev,
    # The code signing certificate (.pfx) and its password, or...
    [string]$CertificatePath,
    [string]$CertificatePassword,
    # ...the thumbprint of a certificate in the certificate store of the user.
    [string]$CertificateThumbprint,
    [string]$TimestampUrl = 'http://timestamp.digicert.com',
    # The Ed25519 private key for updates. Never keep this key in the repo.
    [string]$UpdateKey,
    # The public address of nea.exe in the published release. The default is
    # an address relative to the manifest.
    [string]$PackageUrl,
    [string]$OutDir = 'dist'
)

$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

function Invoke-Native {
    param([string]$File, [string[]]$Arguments)
    # cargo reports progress on stderr; with output redirected, Windows
    # PowerShell would turn each line into an error and 'Stop' would abort.
    $ErrorActionPreference = 'Continue'
    & $File @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$File exited with code $LASTEXITCODE." }
}

function Find-SignTool {
    $onPath = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($onPath) { return $onPath.Source }
    $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    if (-not (Test-Path $kits)) { return $null }
    $found = Get-ChildItem $kits -Filter signtool.exe -Recurse -ErrorAction SilentlyContinue |
        Where-Object { $_.Directory.Name -eq 'x64' } |
        Sort-Object { $_.Directory.Parent.Name } -Descending |
        Select-Object -First 1
    if ($found) { return $found.FullName }
    return $null
}

$cargoToml = Get-Content Cargo.toml -Raw
if ($cargoToml -notmatch '(?m)^version\s*=\s*"([^"]+)"') { throw 'Cargo.toml has no version.' }
$version = $Matches[1]
$profileDir = 'release'
$prefix = 'Nea'
if ($Dev) { $profileDir = 'debug'; $prefix = 'Nea-dev' }

Write-Host "== Nea $version ($profileDir)"
if ($Dev) { Invoke-Native cargo @('build') } else { Invoke-Native cargo @('build', '--release') }
$built = Join-Path "target\$profileDir" 'nea.exe'

if (Test-Path $OutDir) { Remove-Item $OutDir -Recurse -Force }
New-Item -ItemType Directory $OutDir | Out-Null
$exe = Join-Path $OutDir 'nea.exe'
Copy-Item $built $exe

# Code signing comes before the update signature: Ed25519 covers the final bytes.
if ($CertificatePath -or $CertificateThumbprint) {
    $signtool = Find-SignTool
    if (-not $signtool) { throw 'You gave a certificate, but signtool.exe (Windows SDK) is not on this computer.' }
    $signArgs = @('sign', '/fd', 'SHA256', '/td', 'SHA256', '/tr', $TimestampUrl, '/d', 'Nea')
    if ($CertificatePath) {
        $signArgs += @('/f', $CertificatePath)
        if ($CertificatePassword) { $signArgs += @('/p', $CertificatePassword) }
    } else {
        $signArgs += @('/sha1', $CertificateThumbprint)
    }
    Invoke-Native $signtool ($signArgs + @($exe))
    Invoke-Native $signtool @('verify', '/pa', $exe)
    Write-Host "== Signed with signtool"
} else {
    Write-Warning 'No code signing certificate: nea.exe is not signed. SmartScreen shows a warning when a person opens it.'
}

$setup = Join-Path $OutDir "$prefix-$version-setup.exe"
Copy-Item $exe $setup

# Reproducible zip: fixed entry order and timestamps.
$epoch = $env:SOURCE_DATE_EPOCH
if (-not $epoch) { $epoch = (& git log -1 --format=%ct 2>$null) }
if (-not $epoch) { $epoch = 0 }
$stamp = [DateTimeOffset]::FromUnixTimeSeconds([int64]$epoch)
Add-Type -AssemblyName System.IO.Compression
$zipPath = Join-Path (Resolve-Path $OutDir) "$prefix-$version-portable.zip"
$entries = [ordered]@{
    'Nea/LICENSE.txt' = 'LICENSE'
    'Nea/NOTICE.txt'  = 'NOTICE'
    'Nea/nea.exe' = $exe
}
$stream = [System.IO.File]::Open($zipPath, [System.IO.FileMode]::Create)
try {
    $zip = New-Object System.IO.Compression.ZipArchive($stream, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($name in $entries.Keys) {
            $entry = $zip.CreateEntry($name, [System.IO.Compression.CompressionLevel]::Optimal)
            $entry.LastWriteTime = $stamp
            $out = $entry.Open()
            try {
                $bytes = [System.IO.File]::ReadAllBytes((Resolve-Path $entries[$name]))
                $out.Write($bytes, 0, $bytes.Length)
            } finally { $out.Dispose() }
        }
    } finally { $zip.Dispose() }
} finally { $stream.Dispose() }

if ($UpdateKey) {
    $signUpdate = @('run', '--quiet', '--example', 'sign', '--', $exe, $UpdateKey, '--version', $version,
        '--out', (Join-Path $OutDir 'latest.json'))
    if ($PackageUrl) { $signUpdate += @('--url', $PackageUrl) }
    Invoke-Native cargo $signUpdate
    Write-Host "== latest.json signed"
} else {
    Write-Warning 'No -UpdateKey: the script did not make latest.json. The updater cannot find this version.'
}

$sums = Get-ChildItem $OutDir -File | Where-Object { $_.Name -ne 'SHA256SUMS.txt' } | Sort-Object Name | ForEach-Object {
    '{0}  {1}' -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $_.Name
}
[System.IO.File]::WriteAllLines((Join-Path (Resolve-Path $OutDir) 'SHA256SUMS.txt'), [string[]]$sums)

Write-Host "== Done in $OutDir\"
Get-ChildItem $OutDir -File | Sort-Object Name | ForEach-Object { '{0,12:N0}  {1}' -f $_.Length, $_.Name }
