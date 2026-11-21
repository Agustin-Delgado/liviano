//! Memory as Nea measures it: private working set of a
//! process and of every process below it (WebView2's browser, renderers,
//! GPU and utility processes are all children of the app). The same file is
//! used by the example client to measure Edge the same way.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
        },
        ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2},
        Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessEntry {
    pub pid: u32,
    pub parent: u32,
    pub exe: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProcessMemory {
    pub pid: u32,
    pub exe: String,
    /// Private working set, bytes.
    pub private_ws: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct TreeMemory {
    pub root: ProcessMemory,
    /// Every descendant, in the order found.
    pub children: Vec<ProcessMemory>,
    /// Root plus children, bytes.
    pub total: u64,
}

/// The processes below `root`, at any depth. A pid seen twice (a cycle left
/// by pid reuse) is only taken once.
pub fn descendants(entries: &[ProcessEntry], root: u32) -> Vec<&ProcessEntry> {
    let mut by_parent: HashMap<u32, Vec<&ProcessEntry>> = HashMap::new();
    for entry in entries {
        if entry.pid != entry.parent {
            by_parent.entry(entry.parent).or_default().push(entry);
        }
    }
    let mut seen = HashSet::from([root]);
    let mut found = Vec::new();
    let mut pending = vec![root];
    while let Some(pid) = pending.pop() {
        for child in by_parent.get(&pid).into_iter().flatten() {
            if seen.insert(child.pid) {
                found.push(*child);
                pending.push(child.pid);
            }
        }
    }
    found
}

/// Every process running now.
pub fn snapshot() -> Vec<ProcessEntry> {
    let mut entries = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return entries };
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut more = Process32FirstW(snap, &mut entry).is_ok();
        while more {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            entries.push(ProcessEntry {
                pid: entry.th32ProcessID,
                parent: entry.th32ParentProcessID,
                exe: String::from_utf16_lossy(&entry.szExeFile[..len]),
            });
            more = Process32NextW(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    entries
}

/// Private working set of one process, or 0 when it can't be opened (gone,
/// or another user's).
pub fn private_working_set(pid: u32) -> u64 {
    unsafe {
        let Ok(process): windows::core::Result<HANDLE> = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
        else {
            return 0;
        };
        let mut counters = PROCESS_MEMORY_COUNTERS_EX2 {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32,
            ..Default::default()
        };
        let ok = GetProcessMemoryInfo(
            process,
            &mut counters as *mut PROCESS_MEMORY_COUNTERS_EX2 as *mut PROCESS_MEMORY_COUNTERS,
            counters.cb,
        )
        .is_ok();
        let _ = CloseHandle(process);
        if ok { counters.PrivateWorkingSetSize as u64 } else { 0 }
    }
}

/// `root` and all its descendants, measured now.
pub fn measure_tree(root: u32) -> TreeMemory {
    let entries = snapshot();
    let exe = entries.iter().find(|e| e.pid == root).map(|e| e.exe.clone()).unwrap_or_default();
    let root = ProcessMemory { pid: root, exe, private_ws: private_working_set(root) };
    let children: Vec<ProcessMemory> = descendants(&entries, root.pid)
        .into_iter()
        .map(|e| ProcessMemory { pid: e.pid, exe: e.exe.clone(), private_ws: private_working_set(e.pid) })
        .collect();
    let total = root.private_ws + children.iter().map(|c| c.private_ws).sum::<u64>();
    TreeMemory { root, children, total }
}
