<#
.SYNOPSIS
    Compila, firma y empaqueta Liviano en dist\.

.DESCRIPTION
    1. cargo build --release (o debug con -Dev: "Liviano (desarrollo)").
    2. Firma de código con signtool, si se pasa un certificado.
    3. Paquete portable: Liviano-<versión>-portable.zip (liviano.exe, LICENSE, NOTICE).
    4. Instalador por usuario: Liviano-<versión>-setup.exe (el mismo programa;
       por llevar "setup" en el nombre se instala al abrirlo, sin administrador).
    5. Con -UpdateKey: latest.json firmado (Ed25519) para el actualizador.
    6. SHA256SUMS.txt con el hash de cada archivo.

    El zip es reproducible: entradas en orden fijo y con la fecha del último
    commit (SOURCE_DATE_EPOCH si está definida).

.EXAMPLE
    .\build.ps1
.EXAMPLE
    .\build.ps1 -CertificatePath C:\certs\liviano.pfx -CertificatePassword $pwd -UpdateKey keys\production.key `
        -PackageUrl https://github.com/<owner>/liviano/releases/download/v0.2.0/liviano.exe
#>
[CmdletBinding()]
param(
    # Compilación de desarrollo (debug): nombre y título "Liviano (desarrollo)".
    [switch]$Dev,
    # Certificado de firma de código (.pfx) y su contraseña...
    [string]$CertificatePath,
    [string]$CertificatePassword,
    # ...o la huella de uno instalado en el almacén del usuario.
    [string]$CertificateThumbprint,
    [string]$TimestampUrl = 'http://timestamp.digicert.com',
    # Clave privada Ed25519 de las actualizaciones (nunca dentro del repo).
    [string]$UpdateKey,
    # Dirección pública de liviano.exe en la versión publicada; por defecto,
    # relativa al manifiesto.
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
    if ($LASTEXITCODE -ne 0) { throw "$File terminó con código $LASTEXITCODE" }
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
if ($cargoToml -notmatch '(?m)^version\s*=\s*"([^"]+)"') { throw 'No encontré la versión en Cargo.toml' }
$version = $Matches[1]
$profileDir = 'release'
$prefix = 'Liviano'
if ($Dev) { $profileDir = 'debug'; $prefix = 'Liviano-dev' }

Write-Host "== Liviano $version ($profileDir)"
if ($Dev) { Invoke-Native cargo @('build') } else { Invoke-Native cargo @('build', '--release') }
$built = Join-Path "target\$profileDir" 'liviano.exe'

if (Test-Path $OutDir) { Remove-Item $OutDir -Recurse -Force }
New-Item -ItemType Directory $OutDir | Out-Null
$exe = Join-Path $OutDir 'liviano.exe'
Copy-Item $built $exe

# Code signing comes before the update signature: Ed25519 covers the final bytes.
if ($CertificatePath -or $CertificateThumbprint) {
    $signtool = Find-SignTool
    if (-not $signtool) { throw 'Hay certificado pero no encontré signtool.exe (Windows SDK).' }
    $signArgs = @('sign', '/fd', 'SHA256', '/td', 'SHA256', '/tr', $TimestampUrl, '/d', 'Liviano')
    if ($CertificatePath) {
        $signArgs += @('/f', $CertificatePath)
        if ($CertificatePassword) { $signArgs += @('/p', $CertificatePassword) }
    } else {
        $signArgs += @('/sha1', $CertificateThumbprint)
    }
    Invoke-Native $signtool ($signArgs + @($exe))
    Invoke-Native $signtool @('verify', '/pa', $exe)
    Write-Host "== Firmado con signtool"
} else {
    Write-Warning 'Sin certificado de firma de código: liviano.exe no va firmado y SmartScreen va a advertir a quien lo abra.'
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
    'Liviano/LICENSE.txt' = 'LICENSE'
    'Liviano/NOTICE.txt'  = 'NOTICE'
    'Liviano/liviano.exe' = $exe
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
    Write-Host "== latest.json firmado"
} else {
    Write-Warning 'Sin -UpdateKey: no se generó latest.json (el actualizador no va a ver esta versión).'
}

$sums = Get-ChildItem $OutDir -File | Where-Object { $_.Name -ne 'SHA256SUMS.txt' } | Sort-Object Name | ForEach-Object {
    '{0}  {1}' -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $_.Name
}
[System.IO.File]::WriteAllLines((Join-Path (Resolve-Path $OutDir) 'SHA256SUMS.txt'), [string[]]$sums)

Write-Host "== Listo en $OutDir\"
Get-ChildItem $OutDir -File | Sort-Object Name | ForEach-Object { '{0,12:N0}  {1}' -f $_.Length, $_.Name }
