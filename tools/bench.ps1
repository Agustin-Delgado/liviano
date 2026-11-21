<#
.SYNOPSIS
  Client for the Nea script control. It sends one JSON request per line
  to the local pipe of the browser.

.DESCRIPTION
  Use the script as a command (one request for each call):
    tools\bench.ps1 open https://example.com/
    tools\bench.ps1 wait 20000
    tools\bench.ps1 state
    tools\bench.ps1 run toggle_tab_layout
    tools\bench.ps1 keys Ctrl+Shift+S
    tools\bench.ps1 raw '{"cmd":"click","selector":"a"}'

  Use the script as a library (many requests on one connection):
    . tools\bench.ps1
    Connect-Nea
    $tab = (Invoke-Nea open @{ url = 'https://example.com/' }).tab
    Invoke-Nea wait @{ tab = $tab; timeout_ms = 20000 } | Out-Null
    (Invoke-Nea text @{ tab = $tab }).text
    Disconnect-Nea

  Turn on "Script control" in Settings, or start the browser with
  NEA_BENCH=1. NEA_BENCH_PIPE selects a different pipe. The measurement
  scripts use a different pipe, so they do not control the browser that
  you use every day.
#>

$script:NeaConnection = $null

function Get-NeaPipeName {
    param([string]$Suffix = $env:NEA_BENCH_PIPE)
    $clean = { param($t) -join ($t.ToCharArray() | ForEach-Object { if ($_ -match '[A-Za-z0-9_-]') { $_ } else { '_' } }) }
    $user = $env:USERNAME
    if (-not $user) { $user = 'user' }
    $name = 'nea-bench-' + (& $clean $user)
    if ($Suffix -and $Suffix.Trim()) { $name += '-' + (& $clean $Suffix) }
    $name
}

function Connect-Nea {
    param([string]$Pipe = (Get-NeaPipeName), [int]$TimeoutSec = 15)
    Disconnect-Nea
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ($true) {
        $stream = New-Object System.IO.Pipes.NamedPipeClientStream('.', $Pipe, [System.IO.Pipes.PipeDirection]::InOut)
        try {
            $stream.Connect(500)
            break
        } catch {
            $stream.Dispose()
            if ((Get-Date) -gt $deadline) { throw "Cannot open the pipe $Pipe. Make sure that script control is on." }
            Start-Sleep -Milliseconds 200
        }
    }
    $utf8 = New-Object System.Text.UTF8Encoding($false)
    $writer = New-Object System.IO.StreamWriter($stream, $utf8)
    $writer.AutoFlush = $true
    $writer.NewLine = "`n"
    $reader = New-Object System.IO.StreamReader($stream, $utf8)
    $script:NeaConnection = [pscustomobject]@{ Stream = $stream; Writer = $writer; Reader = $reader; Next = 1 }
}

function Disconnect-Nea {
    if ($script:NeaConnection) {
        try { $script:NeaConnection.Stream.Dispose() } catch { }
        $script:NeaConnection = $null
    }
}

# Sends one JSON line and returns the parsed reply (id, ok, result | error).
function Send-NeaLine {
    param([Parameter(Mandatory)][string]$Line)
    if (-not $script:NeaConnection) { Connect-Nea }
    $c = $script:NeaConnection
    $c.Writer.WriteLine($Line)
    $answer = $c.Reader.ReadLine()
    if ($null -eq $answer) { throw 'The browser closed the pipe.' }
    $answer | ConvertFrom-Json
}

function Invoke-Nea {
    param(
        [Parameter(Mandatory, Position = 0)][string]$Cmd,
        [Parameter(Position = 1)][hashtable]$Arguments = @{},
        [switch]$NoThrow
    )
    if (-not $script:NeaConnection) { Connect-Nea }
    $request = [ordered]@{ id = $script:NeaConnection.Next; cmd = $Cmd }
    $script:NeaConnection.Next++
    foreach ($key in $Arguments.Keys) { $request[$key] = $Arguments[$key] }
    $reply = Send-NeaLine ($request | ConvertTo-Json -Compress -Depth 5)
    if (-not $reply.ok -and -not $NoThrow) { throw "Nea ($Cmd): $($reply.error)" }
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
        $reply = Send-NeaLine $text
        $reply | ConvertTo-Json -Compress -Depth 8
        if (-not $reply.ok) { exit 1 }
    } finally {
        Disconnect-Nea
    }
}
