<#
.SYNOPSIS
  Cliente del control por script de Liviano (RF-23): habla con el pipe local
  del navegador, una petición JSON por línea.

.DESCRIPTION
  Uso como comando (una petición por llamada):
    tools\bench.ps1 open https://example.com/
    tools\bench.ps1 wait 20000
    tools\bench.ps1 state
    tools\bench.ps1 run toggle_tab_layout
    tools\bench.ps1 keys Ctrl+Shift+S
    tools\bench.ps1 raw '{"cmd":"click","selector":"a"}'

  Uso como biblioteca (varias peticiones por la misma conexión):
    . tools\bench.ps1
    Connect-Liviano
    $tab = (Invoke-Liviano open @{ url = 'https://example.com/' }).tab
    Invoke-Liviano wait @{ tab = $tab; timeout_ms = 20000 } | Out-Null
    (Invoke-Liviano text @{ tab = $tab }).text
    Disconnect-Liviano

  El navegador tiene que tener activo "Control por script" en Ajustes o
  haberse iniciado con LIVIANO_BENCH=1. LIVIANO_BENCH_PIPE elige otro canal
  (el script de medición lo usa para no chocar con el navegador de uso diario).
#>

$script:LivianoConnection = $null

function Get-LivianoPipeName {
    param([string]$Suffix = $env:LIVIANO_BENCH_PIPE)
    $clean = { param($t) -join ($t.ToCharArray() | ForEach-Object { if ($_ -match '[A-Za-z0-9_-]') { $_ } else { '_' } }) }
    $user = $env:USERNAME
    if (-not $user) { $user = 'user' }
    $name = 'liviano-bench-' + (& $clean $user)
    if ($Suffix -and $Suffix.Trim()) { $name += '-' + (& $clean $Suffix) }
    $name
}

function Connect-Liviano {
    param([string]$Pipe = (Get-LivianoPipeName), [int]$TimeoutSec = 15)
    Disconnect-Liviano
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ($true) {
        $stream = New-Object System.IO.Pipes.NamedPipeClientStream('.', $Pipe, [System.IO.Pipes.PipeDirection]::InOut)
        try {
            $stream.Connect(500)
            break
        } catch {
            $stream.Dispose()
            if ((Get-Date) -gt $deadline) { throw "No se pudo abrir el pipe $Pipe (¿está activo el control por script?)" }
            Start-Sleep -Milliseconds 200
        }
    }
    $utf8 = New-Object System.Text.UTF8Encoding($false)
    $writer = New-Object System.IO.StreamWriter($stream, $utf8)
    $writer.AutoFlush = $true
    $writer.NewLine = "`n"
    $reader = New-Object System.IO.StreamReader($stream, $utf8)
    $script:LivianoConnection = [pscustomobject]@{ Stream = $stream; Writer = $writer; Reader = $reader; Next = 1 }
}

function Disconnect-Liviano {
    if ($script:LivianoConnection) {
        try { $script:LivianoConnection.Stream.Dispose() } catch { }
        $script:LivianoConnection = $null
    }
}

# Sends one JSON line and returns the parsed reply (id, ok, result | error).
function Send-LivianoLine {
    param([Parameter(Mandatory)][string]$Line)
    if (-not $script:LivianoConnection) { Connect-Liviano }
    $c = $script:LivianoConnection
    $c.Writer.WriteLine($Line)
    $answer = $c.Reader.ReadLine()
    if ($null -eq $answer) { throw 'El navegador cerró el pipe' }
    $answer | ConvertFrom-Json
}

function Invoke-Liviano {
    param(
        [Parameter(Mandatory, Position = 0)][string]$Cmd,
        [Parameter(Position = 1)][hashtable]$Arguments = @{},
        [switch]$NoThrow
    )
    if (-not $script:LivianoConnection) { Connect-Liviano }
    $request = [ordered]@{ id = $script:LivianoConnection.Next; cmd = $Cmd }
    $script:LivianoConnection.Next++
    foreach ($key in $Arguments.Keys) { $request[$key] = $Arguments[$key] }
    $reply = Send-LivianoLine ($request | ConvertTo-Json -Compress -Depth 5)
    if (-not $reply.ok -and -not $NoThrow) { throw "Liviano ($Cmd): $($reply.error)" }
    if ($NoThrow) { return $reply }
    $reply.result
}

# Run as a command (not dot-sourced): one request, its reply printed as JSON.
if ($MyInvocation.InvocationName -ne '.' -and $args.Count -gt 0) {
    $command = $args[0]
    $rest = @($args | Select-Object -Skip 1)
    $line = switch ($command) {
        'open'       { @{ cmd = 'open'; url = $rest[0]; background = ($rest -contains '--background') } }
        'wait'       { $w = @{ cmd = 'wait' }; if ($rest.Count -gt 0) { $w.timeout_ms = [int64]$rest[0] }; $w }
        'text'       { @{ cmd = 'text' } }
        'screenshot' { $s = @{ cmd = 'screenshot' }; if ($rest.Count -gt 0) { $s.path = $rest[0] }; $s }
        'click'      { @{ cmd = 'click'; selector = $rest[0] } }
        'run'        { @{ cmd = 'run'; name = $rest[0] } }
        'keys'       { @{ cmd = 'run'; keys = $rest[0] } }
        'type'       { @{ cmd = 'type'; text = $rest[0]; submit = ($rest -contains '--submit') } }
        'close'      { $x = @{ cmd = 'close' }; if ($rest.Count -gt 0) { $x.tab = [int64]$rest[0] }; $x }
        'raw'        { $null }
        default      { @{ cmd = $command } }
    }
    $text = if ($command -eq 'raw') { $rest[0] } else { $line | ConvertTo-Json -Compress }
    try {
        $reply = Send-LivianoLine $text
        $reply | ConvertTo-Json -Compress -Depth 8
        if (-not $reply.ok) { exit 1 }
    } finally {
        Disconnect-Liviano
    }
}
