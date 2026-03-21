#![allow(dead_code)]
use std::fs::{File, Metadata};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;

/// Tracks read position in an append-only file.
/// Handles: partial last lines, file truncation/recreation, new content.
pub struct FileCursor {
    path: PathBuf,
    offset: u64,
    pub line_no: u64,
    partial: Vec<u8>,
    file_identity: Option<u128>,
}

impl FileCursor {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            offset: 0,
            line_no: 0,
            partial: Vec::new(),
            file_identity: None,
        }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Read any new complete lines appended since last call.
    /// Returns Vec of (line_number, line_string) for each complete new line.
    /// Handles partial lines by buffering until newline arrives.
    /// Resets on truncation/recreation.
    pub fn read_new_lines(&mut self) -> Vec<(u64, String)> {
        let mut results = Vec::new();

        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(_) => return results,
        };

        let metadata = match file.metadata() {
            Ok(metadata) => metadata,
            Err(_) => return results,
        };
        let file_len = metadata.len();
        let identity = file_identity(&metadata);

        if self.file_identity != Some(identity) || file_len < self.offset {
            self.offset = 0;
            self.line_no = 0;
            self.partial.clear();
        }
        self.file_identity = Some(identity);

        if file_len == self.offset {
            return results;
        }

        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return results;
        }

        let mut reader = BufReader::new(file);
        loop {
            let mut chunk = Vec::new();
            match reader.read_until(b'\n', &mut chunk) {
                Ok(0) => break,
                Ok(read) => {
                    self.offset += read as u64;
                    self.partial.extend_from_slice(&chunk);

                    if self.partial.last() == Some(&b'\n') {
                        let line = String::from_utf8_lossy(&self.partial)
                            .trim_end_matches(['\n', '\r'])
                            .to_string();
                        self.partial.clear();
                        self.line_no += 1;
                        results.push((self.line_no, line));
                    }
                }
                Err(_) => break,
            }
        }

        results
    }
}

fn file_identity(metadata: &Metadata) -> u128 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        return ((metadata.dev() as u128) << 64) | metadata.ino() as u128;
    }

    #[cfg(not(unix))]
    {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        ((metadata.len() as u128) << 64) ^ modified
    }
}

#[cfg(test)]
mod tests {
    use super::FileCursor;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("imi-{name}-{unique}.log"))
    }

    #[test]
    fn buffers_partial_lines_until_newline() {
        let path = temp_path("partial");
        fs::write(&path, b"first\nsecond").unwrap();

        let mut cursor = FileCursor::new(path.clone());
        assert_eq!(cursor.read_new_lines(), vec![(1, "first".to_string())]);

        fs::write(&path, b"first\nsecond\nthird\n").unwrap();
        assert_eq!(
            cursor.read_new_lines(),
            vec![(2, "second".to_string()), (3, "third".to_string())]
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn resets_after_truncation() {
        let path = temp_path("truncate");
        fs::write(&path, b"one\ntwo\n").unwrap();

        let mut cursor = FileCursor::new(path.clone());
        assert_eq!(cursor.read_new_lines().len(), 2);

        fs::write(&path, b"reset\n").unwrap();
        assert_eq!(cursor.read_new_lines(), vec![(1, "reset".to_string())]);

        let _ = fs::remove_file(path);
    }
}
