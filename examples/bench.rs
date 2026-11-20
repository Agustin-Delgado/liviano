//! Client of the script control pipe (RF-23), and the memory meter the
//! measuring script uses for Edge. It shares the protocol and the meter with
//! the app, file for file.
//!
//!   cargo run --example bench -- open https://example.com/
//!   cargo run --example bench -- wait --timeout 20000
//!   cargo run --example bench -- run --keys Ctrl+Shift+S
//!   cargo run --example bench -- memtree 1234 5678
//!
//! Every reply is printed as one JSON line; the exit code is 1 when the app
//! answered with an error, 2 when the pipe could not be reached.

#[allow(dead_code)]
#[path = "../src/bench/memory.rs"]
mod memory;
#[allow(dead_code)]
#[path = "../src/bench/protocol.rs"]
mod protocol;

use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    process::ExitCode,
    time::{Duration, Instant},
};

use protocol::{Action, Request};
use serde_json::{Value, json};

const USAGE: &str = "uso: bench [--pipe NOMBRE] <comando> [argumentos]
  ping | state | memory | shield | discard | quit
  open <url> [--background]
  wait [--tab N] [--timeout MS]
  text [--tab N]
  screenshot [--tab N] [--path ARCHIVO.png]
  click <selector> [--tab N]
  run <nombre> | run --keys <atajo>
  type <texto> [--submit]
  close [--tab N]
  raw <json>            una petición tal cual
  stdin                 una petición JSON por línea desde la entrada
  memtree <pid> [pid…]  memoria privada de esos procesos y sus hijos (sin el pipe)";

struct Args {
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl Args {
    fn parse(raw: Vec<String>) -> Self {
        let takes_value = ["--tab", "--timeout", "--path", "--keys", "--pipe"];
        let (mut positional, mut flags) = (Vec::new(), Vec::new());
        let mut iter = raw.into_iter();
        while let Some(arg) = iter.next() {
            if takes_value.contains(&arg.as_str()) {
                flags.push((arg, iter.next()));
            } else if arg.starts_with("--") {
                flags.push((arg, None));
            } else {
                positional.push(arg);
            }
        }
        Self { positional, flags }
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.flags.iter().find(|(n, _)| n == name).and_then(|(_, v)| v.as_deref())
    }

    fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|(n, _)| n == name)
    }

    fn number(&self, name: &str) -> Result<Option<u64>, String> {
        self.value(name).map(|v| v.parse::<u64>().map_err(|_| format!("{name} espera un número"))).transpose()
    }
}

fn action(args: &Args) -> Result<Action, String> {
    let word = |i: usize| args.positional.get(i).cloned();
    let tab = args.number("--tab")?;
    let command = word(0).ok_or_else(|| USAGE.to_string())?;
    Ok(match command.as_str() {
        "ping" => Action::Ping,
        "state" => Action::State,
        "memory" => Action::Memory,
        "discard" => Action::Discard,
        "shield" => Action::Shield,
        "quit" => Action::Quit,
        "open" => Action::Open { url: word(1).ok_or("open necesita una dirección")?, background: args.has("--background") },
        "wait" => Action::Wait { tab, timeout_ms: args.number("--timeout")? },
        "text" => Action::Text { tab },
        "screenshot" => Action::Screenshot { tab, path: args.value("--path").map(str::to_string) },
        "click" => Action::Click { tab, selector: word(1).ok_or("click necesita un selector CSS")? },
        "run" => match args.value("--keys") {
            Some(keys) => Action::Run { name: None, keys: Some(keys.to_string()) },
            None => Action::Run { name: Some(word(1).ok_or("run necesita un comando o --keys")?), keys: None },
        },
        "type" => Action::Type { text: word(1).unwrap_or_default(), submit: args.has("--submit") },
        "close" => Action::Close { tab },
        other => return Err(format!("comando desconocido: {other}\n{USAGE}")),
    })
}

fn default_pipe() -> String {
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".to_string());
    protocol::pipe_name(&user, std::env::var("LIVIANO_BENCH_PIPE").ok().as_deref())
}

/// The app may still be starting, or busy with another client: keep trying.
fn connect(name: &str) -> Result<File, String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match OpenOptions::new().read(true).write(true).open(name) {
            Ok(file) => return Ok(file),
            Err(e) if Instant::now() >= deadline => {
                return Err(format!("no se pudo abrir {name}: {e} (¿está activo el control por script?)"));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(150)),
        }
    }
}

fn exchange(file: &File, reader: &mut BufReader<&File>, line: &str) -> Result<Value, String> {
    let mut writer = file;
    writer
        .write_all(line.trim().as_bytes())
        .and_then(|_| writer.write_all(b"\n"))
        .and_then(|_| writer.flush())
        .map_err(|e| format!("no se pudo escribir en el pipe: {e}"))?;
    let mut answer = String::new();
    reader.read_line(&mut answer).map_err(|e| format!("no se pudo leer el pipe: {e}"))?;
    serde_json::from_str(&answer).map_err(|e| format!("respuesta ilegible ({e}): {answer}"))
}

/// Private working set of every listed process and its descendants, each
/// process counted once.
fn memtree(pids: &[u32]) -> Value {
    let entries = memory::snapshot();
    let mut seen = HashSet::new();
    let mut processes = Vec::new();
    for &root in pids {
        let tree = std::iter::once(root).chain(memory::descendants(&entries, root).into_iter().map(|e| e.pid));
        for pid in tree {
            if seen.insert(pid) {
                let exe = entries.iter().find(|e| e.pid == pid).map(|e| e.exe.clone()).unwrap_or_default();
                processes.push(memory::ProcessMemory { pid, exe, private_ws: memory::private_working_set(pid) });
            }
        }
    }
    let total: u64 = processes.iter().map(|p| p.private_ws).sum();
    json!({ "unit": "bytes, private working set", "total": total, "processes": processes })
}

fn main() -> ExitCode {
    let args = Args::parse(std::env::args().skip(1).collect());
    let pipe = args.value("--pipe").map(str::to_string).unwrap_or_else(default_pipe);
    let print_and_code = |reply: &Value| {
        println!("{reply}");
        if reply.get("ok") == Some(&Value::Bool(false)) { ExitCode::from(1) } else { ExitCode::SUCCESS }
    };
    if args.positional.first().map(String::as_str) == Some("memtree") {
        let pids: Result<Vec<u32>, _> = args.positional[1..].iter().map(|p| p.parse::<u32>()).collect();
        return match pids {
            Ok(pids) if !pids.is_empty() => print_and_code(&memtree(&pids)),
            _ => {
                eprintln!("memtree necesita uno o más pid");
                ExitCode::from(2)
            }
        };
    }
    let lines: Vec<String> = match args.positional.first().map(String::as_str) {
        Some("raw") => vec![args.positional.get(1).cloned().unwrap_or_default()],
        Some("stdin") => std::io::stdin().lock().lines().map_while(Result::ok).filter(|l| !l.trim().is_empty()).collect(),
        _ => match action(&args) {
            Ok(action) => vec![serde_json::to_string(&Request { id: json!(1), action }).unwrap_or_default()],
            Err(message) => {
                eprintln!("{message}");
                return ExitCode::from(2);
            }
        },
    };
    let file = match connect(&pipe) {
        Ok(file) => file,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let mut reader = BufReader::new(&file);
    let mut code = ExitCode::SUCCESS;
    for line in lines {
        match exchange(&file, &mut reader, &line) {
            Ok(reply) => {
                if print_and_code(&reply) != ExitCode::SUCCESS {
                    code = ExitCode::from(1);
                }
            }
            Err(message) => {
                eprintln!("{message}");
                return ExitCode::from(2);
            }
        }
    }
    code
}
