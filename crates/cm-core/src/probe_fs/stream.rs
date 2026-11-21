//! Reads of a session record (design section 14, "Reading"). A record is far larger than
//! [`super::MAX_READ_BYTES`], so these stream it instead of holding it. They are not cached and do
//! not count toward the scan budget: a trace operation reads one record, and each read here keeps
//! its own bound.

use std::fs;
use std::io::{self, BufRead, Read, Seek, SeekFrom};

use crate::model::AbsPath;

use super::{Cached, ProbeError, ProbeFs};

/// The bounds of [`ProbeFs::read_lines`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineLimits {
    /// Bytes read from one file at most.
    pub record: u64,
    /// A longer line is skipped whole and counted.
    pub line: u64,
}

impl Default for LineLimits {
    fn default() -> LineLimits {
        LineLimits {
            record: 512 * 1024 * 1024,
            line: 64 * 1024 * 1024,
        }
    }
}

/// What [`ProbeFs::read_lines`] read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LinesRead {
    /// Bytes read from the file.
    pub bytes: u64,
    /// The file's length, when the read stopped at [`LineLimits::record`] before its end.
    pub stopped_at_limit: Option<u64>,
    /// Lines over [`LineLimits::line`], skipped.
    pub long_lines: u32,
}

/// A part of a file, with the file's length when it was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSlice {
    pub bytes: Vec<u8>,
    pub len: u64,
}

/// Each `fill_buf` hands the line reader at most this much.
const BUFFER: usize = 1024 * 1024;

impl ProbeFs {
    /// A probe whose line reads stop at `limits` instead of the default ones.
    pub fn with_line_limits(limits: LineLimits) -> ProbeFs {
        ProbeFs {
            line_limits: limits,
            ..ProbeFs::default()
        }
    }

    pub fn line_limits(&self) -> LineLimits {
        self.line_limits
    }

    /// Calls `each` with every line of a regular file, without its newline, in order, and with
    /// whether a newline ended it. Only the last line can lack one. It reads at most
    /// [`LineLimits::record`] bytes and drops the line the limit cuts. It skips a line over
    /// [`LineLimits::line`] without holding it.
    pub fn read_lines(
        &self,
        path: &AbsPath,
        each: &mut dyn FnMut(&[u8], bool),
    ) -> Cached<LinesRead> {
        let result = stream_lines(path, self.line_limits, each);
        self.note_probe(path, result.is_ok());
        result
    }

    /// At most `max` bytes from the start of a regular file.
    pub fn read_head(&self, path: &AbsPath, max: u64) -> Cached<FileSlice> {
        let result = slice(path, max, false);
        self.note_probe(path, result.is_ok());
        result
    }

    /// At most `max` bytes from the end of a regular file.
    pub fn read_tail(&self, path: &AbsPath, max: u64) -> Cached<FileSlice> {
        let result = slice(path, max, true);
        self.note_probe(path, result.is_ok());
        result
    }

    fn note_probe(&self, path: &AbsPath, hit: bool) {
        let mut state = self.state.lock().expect("probe state");
        state.probes.insert(path.clone(), hit);
    }
}

/// Opens a regular file. Checks the kind first, as [`super::read_regular`] does: opening a named
/// pipe blocks until a writer comes.
fn open_regular(path: &AbsPath) -> Cached<fs::File> {
    if !fs::metadata(path.as_path())?.is_file() {
        return Err(ProbeError::not_regular());
    }
    Ok(fs::File::open(path.as_path())?)
}

fn stream_lines(
    path: &AbsPath,
    limits: LineLimits,
    each: &mut dyn FnMut(&[u8], bool),
) -> Cached<LinesRead> {
    let file = open_regular(path)?;
    let mut reader = io::BufReader::with_capacity(BUFFER, file.take(limits.record));
    let mut out = LinesRead::default();
    let mut line: Vec<u8> = Vec::new();
    let mut skipping = false;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            break;
        }
        let newline = buf.iter().position(|&b| b == b'\n');
        let chunk = &buf[..newline.unwrap_or(buf.len())];
        if !skipping && (line.len() + chunk.len()) as u64 > limits.line {
            skipping = true;
            out.long_lines += 1;
            line = Vec::new();
        }
        let used = newline.map_or(buf.len(), |i| i + 1);
        if newline.is_some() && !skipping {
            if line.is_empty() {
                each(chunk, true);
            } else {
                line.extend_from_slice(chunk);
                each(&line, true);
                line.clear();
            }
        } else if !skipping {
            line.extend_from_slice(chunk);
        }
        if newline.is_some() {
            skipping = false;
        }
        reader.consume(used);
        out.bytes += used as u64;
    }
    let len = reader.get_ref().get_ref().metadata()?.len();
    if out.bytes >= limits.record && len > out.bytes {
        out.stopped_at_limit = Some(len);
    } else if !line.is_empty() && !skipping {
        each(&line, false);
    }
    Ok(out)
}

fn slice(path: &AbsPath, max: u64, from_end: bool) -> Cached<FileSlice> {
    let mut file = open_regular(path)?;
    let len = file.metadata()?.len();
    let start = if from_end { len.saturating_sub(max) } else { 0 };
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity(max.min(len) as usize);
    file.take(max).read_to_end(&mut bytes)?;
    Ok(FileSlice { bytes, len })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &tempfile::TempDir, bytes: &[u8]) -> AbsPath {
        let path = dir.path().canonicalize().unwrap().join("r.jsonl");
        std::fs::write(&path, bytes).unwrap();
        AbsPath::parse(path.to_str().unwrap()).unwrap()
    }

    fn lines(fs: &ProbeFs, path: &AbsPath) -> (Vec<(String, bool)>, LinesRead) {
        let mut seen = Vec::new();
        let read = fs
            .read_lines(path, &mut |line, ended| {
                seen.push((String::from_utf8(line.to_vec()).unwrap(), ended))
            })
            .unwrap();
        (seen, read)
    }

    #[test]
    fn lines_come_whole_and_the_last_says_it_has_no_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, b"one\n\ntwo\nthr");
        let (seen, read) = lines(&ProbeFs::new(), &path);
        let want = [("one", true), ("", true), ("two", true), ("thr", false)];
        let want: Vec<_> = want.iter().map(|(s, e)| (s.to_string(), *e)).collect();
        assert_eq!(seen, want);
        assert_eq!(read.bytes, 12);
        assert_eq!(read.stopped_at_limit, None);
    }

    #[test]
    fn a_long_line_is_skipped_and_counted() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, b"ok\nthis is too long\nok again\n");
        let fs = ProbeFs::with_line_limits(LineLimits {
            record: 1000,
            line: 8,
        });
        let (seen, read) = lines(&fs, &path);
        let texts: Vec<_> = seen.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(texts, ["ok", "ok again"]);
        assert_eq!(read.long_lines, 1);
    }

    #[test]
    fn the_record_limit_stops_the_read_and_drops_the_line_it_cuts() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, b"aaaa\nbbbb\ncccc\n");
        let fs = ProbeFs::with_line_limits(LineLimits {
            record: 12,
            line: 100,
        });
        let (seen, read) = lines(&fs, &path);
        let texts: Vec<_> = seen.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(texts, ["aaaa", "bbbb"]);
        assert_eq!(read.bytes, 12);
        assert_eq!(read.stopped_at_limit, Some(15));
    }

    #[test]
    fn head_and_tail_read_at_most_their_bound() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, b"0123456789");
        let fs = ProbeFs::new();
        assert_eq!(
            fs.read_head(&path, 4).unwrap(),
            FileSlice {
                bytes: b"0123".to_vec(),
                len: 10
            }
        );
        assert_eq!(fs.read_tail(&path, 3).unwrap().bytes, b"789");
        assert_eq!(fs.read_tail(&path, 30).unwrap().bytes, b"0123456789");
        let missing = AbsPath::parse(&format!("{}/none", dir.path().display())).unwrap();
        assert!(fs.read_head(&missing, 4).unwrap_err().is_not_found());
    }

    #[test]
    fn a_directory_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = AbsPath::parse(dir.path().to_str().unwrap()).unwrap();
        let err = ProbeFs::new()
            .read_lines(&path, &mut |_, _| {})
            .unwrap_err();
        assert_eq!(err.kind, super::super::ProbeErrorKind::NotRegular);
    }
}
