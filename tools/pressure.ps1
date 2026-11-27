# Makes this computer behave as a computer with less memory, for a test.
#
# -LeaveFreeMB: the script takes physical memory until only this quantity
#   stays available. It fills the memory with random data and writes to it
#   again and again, so the system cannot compress it or move it to the
#   page file. The system then takes memory from the other processes.
# -CommitMB: the script also commits this quantity and does not use it.
#   This increases the commit of the system and uses no physical memory.
# -Seconds: the script stops after this time. It also stops when the file
#   -StopFile exists.
#
# The script never leaves less than 1024 MB available. It also takes no
# more memory when less than 4096 MB of commit stay free in the system.

param(
    [int]$LeaveFreeMB = 2048,
    [int]$CommitMB = 0,
    [int]$Seconds = 300,
    [string]$StopFile = "$env:TEMP\nea-pressure.stop"
)

$ErrorActionPreference = "Stop"
if ($LeaveFreeMB -lt 1024) { throw "LeaveFreeMB must be 1024 or more" }
if (Test-Path $StopFile) { Remove-Item $StopFile }

Add-Type -TypeDefinition @"
using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading;

public static class NeaPressure {
    [StructLayout(LayoutKind.Sequential)]
    struct MemoryStatus {
        public uint length, load;
        public ulong totalPhys, availPhys, totalPage, availPage, totalVirtual, availVirtual, availExtended;
    }
    [DllImport("kernel32.dll")] static extern bool GlobalMemoryStatusEx(ref MemoryStatus status);
    [DllImport("kernel32.dll")] static extern IntPtr VirtualAlloc(IntPtr address, UIntPtr size, uint type, uint protect);

    const int ChunkMb = 256;
    const int Page = 4096;
    // The script stops before the commit of the system is full: a full
    // commit makes the other programs fail.
    const int MinCommitFreeMb = 4096;

    static MemoryStatus Status() {
        var status = new MemoryStatus();
        status.length = (uint)Marshal.SizeOf(typeof(MemoryStatus));
        GlobalMemoryStatusEx(ref status);
        return status;
    }

    public static void Run(int leaveFreeMb, int commitMb, int seconds, string stopFile) {
        if (commitMb > 0) {
            // MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE. Memory that is not used.
            var held = VirtualAlloc(IntPtr.Zero, (UIntPtr)((ulong)commitMb << 20), 0x3000, 0x04);
            Console.WriteLine(held == IntPtr.Zero ? "commit failed" : "committed " + commitMb + " MB");
        }
        var chunks = new List<byte[]>();
        var random = new Random(1);
        var end = DateTime.UtcNow.AddSeconds(seconds);
        var report = DateTime.UtcNow;
        byte mark = 0;
        while (DateTime.UtcNow < end && !File.Exists(stopFile)) {
            var status = Status();
            long free = (long)(status.availPhys >> 20);
            if (free > leaveFreeMb + ChunkMb && (long)(status.availPage >> 20) > MinCommitFreeMb + ChunkMb) {
                var chunk = new byte[ChunkMb << 20];
                random.NextBytes(chunk);
                chunks.Add(chunk);
            } else {
                Thread.Sleep(500);
            }
            mark++;
            foreach (var chunk in chunks)
                for (int at = 0; at < chunk.Length; at += Page) chunk[at] = mark;
            if (DateTime.UtcNow >= report) {
                status = Status();
                Console.WriteLine("holds " + (chunks.Count * ChunkMb) + " MB, available " + (status.availPhys >> 20)
                    + " MB, load " + status.load + " %, commit "
                    + (100 - 100 * (long)(status.availPage >> 20) / (long)(status.totalPage >> 20)) + " %");
                report = DateTime.UtcNow.AddSeconds(15);
            }
        }
    }
}
"@

[NeaPressure]::Run($LeaveFreeMB, $CommitMB, $Seconds, $StopFile)
