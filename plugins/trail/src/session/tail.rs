//! Incremental reading of an append-only JSONL file: each call hands over only the complete lines
//! appended since the last one, with their byte offsets.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::SystemTime;

/// How much of line 1 `header_changed` reads.
const HEADER_MAX: u64 = 4096;

#[derive(Debug, Default)]
pub struct Tail {
    offset: u64,
    /// Bytes after the last newline: a line still being written.
    pending: Vec<u8>,
    mtime: Option<SystemTime>,
    first_line: Vec<u8>,
}

impl Tail {
    /// Calls `f(offset, line)` for every complete line appended since the last call. A file that
    /// shrank was replaced, so it is read again from the start.
    pub fn read_new(&mut self, path: &Path, mut f: impl FnMut(u64, &[u8])) {
        let Ok(mut file) = File::open(path) else { return };
        let Ok(len) = file.metadata().map(|m| m.len()) else { return };
        if len < self.offset {
            self.offset = 0;
            self.pending.clear();
        }
        if len == self.offset || file.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut start = self.offset - self.pending.len() as u64;
        let mut bytes = std::mem::take(&mut self.pending);
        let Ok(read) = file.take(len - self.offset).read_to_end(&mut bytes) else { return };
        self.offset += read as u64;
        let complete = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        self.pending = bytes.split_off(complete);
        for line in bytes.split_inclusive(|&b| b == b'\n') {
            let text = line.strip_suffix(b"\n").unwrap_or(line);
            if !text.is_empty() {
                f(start, text);
            }
            start += line.len() as u64;
        }
    }

    /// Line 1 when the file changed since the last call and its first line reads differently: omp
    /// rewrites a padded title record there in place.
    pub fn header_changed(&mut self, path: &Path) -> Option<Vec<u8>> {
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
        if self.mtime == Some(mtime) {
            return None;
        }
        self.mtime = Some(mtime);
        let mut head = Vec::new();
        File::open(path).ok()?.take(HEADER_MAX).read_to_end(&mut head).ok()?;
        let line = head.split(|&b| b == b'\n').next().unwrap_or_default().to_vec();
        if line == self.first_line {
            return None;
        }
        self.first_line.clone_from(&line);
        Some(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn reads_appends_and_holds_torn_lines() {
        let dir = std::env::temp_dir().join(format!("rex-trail-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        std::fs::write(&path, "one\ntw").unwrap();
        let mut t = Tail::default();
        let mut seen = Vec::new();
        t.read_new(&path, |off, l| seen.push((off, String::from_utf8_lossy(l).to_string())));
        assert_eq!(seen, [(0, "one".to_string())], "the torn line waits");

        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "o\nthree").unwrap();
        seen.clear();
        t.read_new(&path, |off, l| seen.push((off, String::from_utf8_lossy(l).to_string())));
        assert_eq!(seen, [(4, "two".to_string()), (8, "three".to_string())]);

        std::fs::write(&path, "new\n").unwrap();
        seen.clear();
        t.read_new(&path, |off, l| seen.push((off, String::from_utf8_lossy(l).to_string())));
        assert_eq!(seen, [(0, "new".to_string())], "a replaced file is read from the start");
        assert_eq!(t.header_changed(&path), Some(b"new".to_vec()));
        assert_eq!(t.header_changed(&path), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
