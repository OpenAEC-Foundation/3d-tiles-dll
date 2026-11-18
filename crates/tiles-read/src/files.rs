//! Where a tileset's files come from: a folder, a zip archive, or files held in memory.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::path::PathBuf;

/// The files of a tileset, by their path relative to the root (with `/` between the parts).
pub trait Files {
    fn read(&self, path: &str) -> Result<Vec<u8>, String>;
    /// Every file there is (used to find the tileset in an archive and the tiles of terrain).
    fn list(&self) -> Vec<String>;
}

/// A folder on disk.
pub struct Dir(pub PathBuf);

impl Files for Dir {
    fn read(&self, path: &str) -> Result<Vec<u8>, String> {
        let file = self.0.join(path);
        std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))
    }
    fn list(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut todo = vec![(self.0.clone(), String::new())];
        while let Some((dir, prefix)) = todo.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
                if entry.path().is_dir() {
                    if out.len() < 1_000_000 {
                        todo.push((entry.path(), format!("{name}/")));
                    }
                } else {
                    out.push(name);
                }
            }
        }
        out.sort();
        out
    }
}

/// A zip archive held in memory.
pub struct Zip(RefCell<zip::ZipArchive<Cursor<Vec<u8>>>>);

impl Zip {
    pub fn new(bytes: Vec<u8>) -> Result<Zip, String> {
        zip::ZipArchive::new(Cursor::new(bytes)).map(|z| Zip(RefCell::new(z))).map_err(|e| format!("zip: {e}"))
    }
}

impl Files for Zip {
    fn read(&self, path: &str) -> Result<Vec<u8>, String> {
        let mut archive = self.0.borrow_mut();
        let mut file = archive.by_name(path).map_err(|e| format!("{path}: {e}"))?;
        let mut out = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut out).map_err(|e| format!("{path}: {e}"))?;
        Ok(out)
    }
    fn list(&self) -> Vec<String> {
        let archive = self.0.borrow();
        let mut out: Vec<String> = archive.file_names().filter_map(|n| n.ok()).filter(|n| !n.ends_with('/')).map(|n| n.into_owned()).collect();
        out.sort();
        out
    }
}

/// Files held in memory, by path.
#[derive(Default)]
pub struct Memory(pub BTreeMap<String, Vec<u8>>);

impl Files for Memory {
    fn read(&self, path: &str) -> Result<Vec<u8>, String> {
        self.0.get(path).cloned().ok_or_else(|| format!("{path}: not here (only the file itself was given)"))
    }
    fn list(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }
}

/// The folder part of a path (`a/b/c.json` gives `a/b/`).
pub fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..=i],
        None => "",
    }
}

/// A URI of the tileset resolved against the folder it is written in: query and fragment
/// dropped, percent escapes decoded, `.` and `..` worked out. URLs that leave the tileset
/// (`http:` and the like) are refused: streaming is not supported.
pub fn join(dir: &str, uri: &str) -> Result<String, String> {
    let uri = uri.split(['?', '#']).next().unwrap_or("");
    if uri.contains("://") || uri.starts_with("//") {
        return Err(format!("{uri}: tiles on a server are not read (only a tileset on disk or in a zip)"));
    }
    let uri = percent_decode(uri);
    let mut parts: Vec<&str> = if uri.starts_with('/') { Vec::new() } else { dir.split('/').filter(|p| !p.is_empty()).collect() };
    for part in uri.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    Ok(parts.join("/"))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_uris() {
        assert_eq!(join("a/b/", "../c/d.b3dm?v=2").unwrap(), "a/c/d.b3dm");
        assert_eq!(join("", "./tiles/0%201.glb").unwrap(), "tiles/0 1.glb");
        assert!(join("", "https://example.org/t.b3dm").is_err());
        assert_eq!(dir_of("x/y/tileset.json"), "x/y/");
    }
}
