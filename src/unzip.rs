//! Just enough zip to unpack an extension: the central directory, stored and
//! deflated entries, no zip64, no encryption. Sizes come from the central
//! directory, since store packages write them after the data (bit 3).

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

/// Extensions are small; this stops a hostile archive from filling the disk.
const MAX_TOTAL: u64 = 1 << 30;
const MAX_ENTRIES: usize = 100_000;

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

#[derive(Debug, PartialEq, Eq)]
pub struct Entry<'a> {
    pub name: String,
    pub method: u16,
    pub data: &'a [u8],
    pub size: u64,
}

impl Entry<'_> {
    pub fn is_dir(&self) -> bool {
        self.name.ends_with('/')
    }

    pub fn contents(&self) -> Result<Vec<u8>, String> {
        match self.method {
            0 => Ok(self.data.to_vec()),
            8 => {
                let bytes = miniz_oxide::inflate::decompress_to_vec_with_limit(self.data, self.size as usize)
                    .map_err(|_| format!("{} está dañado", self.name))?;
                if bytes.len() as u64 != self.size {
                    return Err(format!("{} no tiene el tamaño declarado", self.name));
                }
                Ok(bytes)
            }
            other => Err(format!("{}: compresión {other} no soportada", self.name)),
        }
    }
}

/// The archive's entries, in central directory order.
pub fn entries(archive: &[u8]) -> Result<Vec<Entry<'_>>, String> {
    let malformed = || "el archivo zip está dañado".to_string();
    // End of central directory: 22 bytes plus a comment of up to 64 KB.
    let lowest = archive.len().saturating_sub(22 + 0xffff);
    let end = (lowest..=archive.len().saturating_sub(22))
        .rev()
        .find(|&at| u32_at(archive, at) == Some(0x0605_4b50))
        .ok_or_else(malformed)?;
    let count = u16_at(archive, end + 10).ok_or_else(malformed)? as usize;
    let directory = u32_at(archive, end + 16).ok_or_else(malformed)?;
    if directory == u32::MAX || count == 0xffff {
        return Err("zip64 no soportado".to_string());
    }
    if count > MAX_ENTRIES {
        return Err(malformed());
    }
    let mut at = directory as usize;
    let mut out = Vec::with_capacity(count);
    let mut total = 0u64;
    for _ in 0..count {
        if u32_at(archive, at) != Some(0x0201_4b50) {
            return Err(malformed());
        }
        let flags = u16_at(archive, at + 8).ok_or_else(malformed)?;
        let method = u16_at(archive, at + 10).ok_or_else(malformed)?;
        let compressed = u32_at(archive, at + 20).ok_or_else(malformed)? as usize;
        let size = u32_at(archive, at + 24).ok_or_else(malformed)? as u64;
        let name_len = u16_at(archive, at + 28).ok_or_else(malformed)? as usize;
        let extra_len = u16_at(archive, at + 30).ok_or_else(malformed)? as usize;
        let comment_len = u16_at(archive, at + 32).ok_or_else(malformed)? as usize;
        let local = u32_at(archive, at + 42).ok_or_else(malformed)? as usize;
        let name = archive.get(at + 46..at + 46 + name_len).ok_or_else(malformed)?;
        // Bit 11: UTF-8 names; otherwise CP437, which for the ASCII names
        // extensions use is the same thing.
        let name = String::from_utf8_lossy(name).into_owned();
        if flags & 1 != 0 {
            return Err(format!("{name} está cifrado"));
        }
        if u32_at(archive, local) != Some(0x0403_4b50) {
            return Err(malformed());
        }
        let local_name = u16_at(archive, local + 26).ok_or_else(malformed)? as usize;
        let local_extra = u16_at(archive, local + 28).ok_or_else(malformed)? as usize;
        let start = local + 30 + local_name + local_extra;
        let data = archive.get(start..start + compressed).ok_or_else(malformed)?;
        total += size;
        if total > MAX_TOTAL {
            return Err("el contenido descomprimido es demasiado grande".to_string());
        }
        out.push(Entry { name, method, data, size });
        at += 46 + name_len + extra_len + comment_len;
    }
    Ok(out)
}

/// Where an entry may be written: relative, inside the folder, no drive, no
/// "..". Anything else is refused rather than cleaned up.
pub fn safe_path(name: &str) -> Option<PathBuf> {
    let name = name.replace('\\', "/");
    let name = name.trim_end_matches('/');
    if name.is_empty() || name.starts_with('/') || name.contains(':') {
        return None;
    }
    let path = PathBuf::from(name);
    path.components().all(|c| matches!(c, Component::Normal(_))).then_some(path)
}

/// Unpacks the whole archive into `dest` (created if missing). Returns how
/// many files were written.
pub fn extract(archive: &[u8], dest: &Path) -> Result<usize, String> {
    let io = |e: std::io::Error| format!("no se pudo escribir la extensión: {e}");
    fs::create_dir_all(dest).map_err(io)?;
    let mut files = 0;
    for entry in entries(archive)? {
        let relative = safe_path(&entry.name).ok_or_else(|| format!("ruta no permitida en el paquete: {}", entry.name))?;
        let target = dest.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(io)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(io)?;
        }
        fs::write(&target, entry.contents()?).map_err(io)?;
        files += 1;
    }
    Ok(files)
}

