<#
.SYNOPSIS
  Turns the module+offset lines of a Nea crash.log into functions, files
  and lines, with the PDB of the same build.

.DESCRIPTION
  crash.log writes each frame of a panic or an exception as
  "nea.exe+0x1A2B3C". The release build has no symbols in it, but each
  release publishes the PDB of its nea.exe as nea-<version>.pdb next to
  the other files. The crash.log line has the version ("panic v0.1.3",
  "exception v0.1.3"): use the PDB of that version. A PDB of another build
  gives wrong names without a warning.

  The script uses dbghelp.dll of Windows. It needs no other tool and no
  connection: it reads only the PDB that you give.

  Without -Log and -Offset, it reads crash.log from the standard input.

.EXAMPLE
  tools\symbolize.ps1 -Pdb nea-0.1.3.pdb -Log $env:APPDATA\Nea\crash.log
.EXAMPLE
  tools\symbolize.ps1 -Pdb target\release\nea.pdb -Offset 0x1A2B3C, 0x4D5E
.EXAMPLE
  Get-Content crash.log | tools\symbolize.ps1 -Pdb nea-0.1.3.pdb
#>
param(
    [Parameter(Mandatory)][string]$Pdb,
    [string]$Log = '',
    # Offsets in nea.exe, as crash.log writes them: 0x1A2B3C, 'nea.exe+0x1A2B3C'
    # or '1A2B3C' (a string without 0x is hexadecimal too).
    [object[]]$Offset = @(),
    # The module name whose offsets this PDB resolves.
    [string]$Module = 'nea.exe',
    [Parameter(ValueFromPipeline)][string]$InputLine
)

begin {
    $ErrorActionPreference = 'Stop'
    $Pdb = (Resolve-Path -LiteralPath $Pdb).Path
    if (-not ('NeaSymbols' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class NeaSymbols {
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct SYMBOL_INFOW {
        public uint SizeOfStruct; public uint TypeIndex; public ulong Reserved0; public ulong Reserved1;
        public uint Index; public uint Size; public ulong ModBase; public uint Flags; public ulong Value;
        public ulong Address; public uint Register; public uint Scope; public uint Tag; public uint NameLen;
        public uint MaxNameLen;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 1024)] public string Name;
    }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct IMAGEHLP_LINEW64 {
        public uint SizeOfStruct; public IntPtr Key; public uint LineNumber; public IntPtr FileName; public ulong Address;
    }
    [DllImport("dbghelp.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool SymInitializeW(IntPtr process, string searchPath, bool invade);
    [DllImport("dbghelp.dll", SetLastError = true)]
    static extern uint SymSetOptions(uint options);
    [DllImport("dbghelp.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern ulong SymLoadModuleExW(IntPtr process, IntPtr file, string image, string module, ulong baseOfDll, uint dllSize, IntPtr data, uint flags);
    [DllImport("dbghelp.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool SymFromAddrW(IntPtr process, ulong address, out ulong displacement, ref SYMBOL_INFOW symbol);
    [DllImport("dbghelp.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool SymGetLineFromAddrW64(IntPtr process, ulong address, out uint displacement, ref IMAGEHLP_LINEW64 line);
    [DllImport("dbghelp.dll", SetLastError = true)]
    static extern bool SymCleanup(IntPtr process);

    // Any value works as the session handle when nothing is invaded.
    static readonly IntPtr Session = new IntPtr(0x4E4541);
    public const ulong Base = 0x140000000;

    public static void Open(string pdb) {
        const uint SYMOPT_UNDNAME = 0x2, SYMOPT_DEFERRED_LOADS = 0x4, SYMOPT_LOAD_LINES = 0x10, SYMOPT_FAIL_CRITICAL_ERRORS = 0x200;
        SymSetOptions(SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS | SYMOPT_LOAD_LINES | SYMOPT_FAIL_CRITICAL_ERRORS);
        if (!SymInitializeW(Session, null, false)) throw new Exception("SymInitialize failed: " + Marshal.GetLastWin32Error());
        // A PDB loads as a module of its own at the base given; its size is
        // only the span of addresses it answers for.
        if (SymLoadModuleExW(Session, IntPtr.Zero, pdb, null, Base, 0x40000000, IntPtr.Zero, 0) == 0)
            throw new Exception("The PDB did not load: " + Marshal.GetLastWin32Error());
    }

    public static string Lookup(ulong offset) {
        ulong address = Base + offset;
        var symbol = new SYMBOL_INFOW();
        symbol.SizeOfStruct = 88;
        symbol.MaxNameLen = 1024;
        ulong displacement;
        if (!SymFromAddrW(Session, address, out displacement, ref symbol)) return null;
        // Past the last function (outside the code), the nearest symbol
        // before the address is no answer.
        if (displacement > 0x100000) return null;
        var text = new StringBuilder(symbol.Name);
        text.AppendFormat(" + 0x{0:X}", displacement);
        var line = new IMAGEHLP_LINEW64();
        line.SizeOfStruct = (uint)Marshal.SizeOf(typeof(IMAGEHLP_LINEW64));
        uint column;
        if (SymGetLineFromAddrW64(Session, address, out column, ref line))
            text.AppendFormat("  ({0}:{1})", Marshal.PtrToStringUni(line.FileName), line.LineNumber);
        return text.ToString();
    }

    public static void Close() { SymCleanup(Session); }
}
'@
    }
    [NeaSymbols]::Open($Pdb)
    $pattern = [regex]('(?i)' + [regex]::Escape($Module) + '\+0x([0-9A-F]+)')
    function Format-Line([string]$text) {
        $found = $pattern.Matches($text)
        if ($found.Count -eq 0) { return $text }
        $names = foreach ($m in $found) {
            $name = [NeaSymbols]::Lookup([Convert]::ToUInt64($m.Groups[1].Value, 16))
            if ($name) { $name } else { '?' }
        }
        "$text  ->  $($names -join '; ')"
    }
    $fromPipe = New-Object System.Collections.Generic.List[string]
}

process {
    if ($null -ne $InputLine) { $fromPipe.Add($InputLine) }
}

end {
    try {
        if ($Offset.Count -gt 0) {
            foreach ($o in $Offset) {
                # PowerShell reads a bare 0x1A2B3C as a number before it gets here.
                $value = if ($o -is [string]) {
                    [Convert]::ToUInt64(($o.Trim() -replace '^(?i)(.*\+)?0x', ''), 16)
                } else { [uint64]$o }
                $name = [NeaSymbols]::Lookup($value)
                if (-not $name) { $name = '?' }
                '{0}+0x{1:X}  ->  {2}' -f $Module, $value, $name
            }
        } elseif ($Log) {
            Get-Content -LiteralPath $Log | ForEach-Object { Format-Line $_ }
        } else {
            $fromPipe | ForEach-Object { Format-Line $_ }
        }
    } finally {
        [NeaSymbols]::Close()
    }
}
