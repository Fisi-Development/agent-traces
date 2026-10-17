//! Streaming, permissive JSON Lines readers shared by store adapters.
use crate::context::Context;
use serde_json::Value;
use std::{
    fs::File,
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

// Codex compresses idle rollouts in place to `<name>.jsonl.zst` while its index keeps
// the `.jsonl` path, so a missing `.jsonl` falls back to its `.zst` sibling.
pub fn locate(path: &Path) -> PathBuf {
    if path.exists() {
        return path.to_owned();
    }
    let mut zst = path.as_os_str().to_owned();
    zst.push(".zst");
    let zst = PathBuf::from(zst);
    if zst.exists() { zst } else { path.to_owned() }
}

pub fn exists(path: &Path) -> bool {
    locate(path).exists()
}

fn is_zst(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("zst"))
}

type Frame = ruzstd::decoding::StreamingDecoder<BufReader<File>, ruzstd::decoding::FrameDecoder>;

// Streams a zstd file frame by frame: a stream may hold several concatenated frames,
// and some rollouts are tens of megabytes compressed, so nothing is held whole.
struct Frames(Option<Frame>);

impl Frames {
    fn open(path: &Path) -> io::Result<Self> {
        let src = BufReader::with_capacity(128 * 1024, File::open(path)?);
        Ok(Self(Some(
            ruzstd::decoding::StreamingDecoder::new(src).map_err(io::Error::other)?,
        )))
    }
}

impl Read for Frames {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let Some(frame) = self.0.as_mut() else {
                return Ok(0);
            };
            let n = frame.read(buf)?;
            if n > 0 || buf.is_empty() {
                return Ok(n);
            }
            let (mut src, decoder) = self.0.take().unwrap().into_parts();
            if src.fill_buf()?.is_empty() {
                return Ok(0);
            }
            self.0 = Some(
                ruzstd::decoding::StreamingDecoder::new_with_decoder(src, decoder)
                    .map_err(io::Error::other)?,
            );
        }
    }
}

/// Opens a JSON Lines store file, plain or zstd-compressed, at `path` or its `.zst` sibling.
pub fn open(path: &Path) -> io::Result<Box<dyn BufRead>> {
    let real = locate(path);
    if is_zst(&real) {
        Ok(Box::new(BufReader::with_capacity(
            128 * 1024,
            Frames::open(&real)?,
        )))
    } else {
        Ok(Box::new(BufReader::with_capacity(
            128 * 1024,
            File::open(real)?,
        )))
    }
}

// Python's json.loads(strict=False) accepts literal control bytes inside strings.
pub fn decode(raw: &[u8]) -> serde_json::Result<Value> {
    if let Ok(value) = serde_json::from_slice(raw) {
        return Ok(value);
    }
    let text = String::from_utf8_lossy(raw);
    let mut clean = String::with_capacity(text.len());
    let (mut quoted, mut escaped) = (false, false);
    for c in text.chars() {
        if quoted && !escaped && (c as u32) < 32 {
            clean.push_str(&format!("\\u{:04x}", c as u32));
            continue;
        }
        clean.push(c);
        if escaped {
            escaped = false;
        } else if c == '\\' && quoted {
            escaped = true;
        } else if c == '"' {
            quoted = !quoted;
        }
    }
    serde_json::from_str(&clean)
}

pub fn records<'a>(
    ctx: &'a Context,
    path: &Path,
    start: usize,
    needles: Option<&[String]>,
) -> Box<dyn Iterator<Item = (usize, Value)> + 'a> {
    ctx.protect_source(path);
    let mut reader = match open(path) {
        Ok(r) => r,
        Err(e) => {
            ctx.diagnostic("store_unreadable", None, Some(path), e.to_string());
            return Box::new(std::iter::empty());
        }
    };
    let path = path.to_owned();
    let needles = needles
        .unwrap_or_default()
        .iter()
        .map(|n| n.as_bytes().to_vec())
        .collect::<Vec<_>>();
    let mut raw = Vec::new();
    let mut index = 0;
    let mut finished = false;
    Box::new(std::iter::from_fn(move || {
        while !finished {
            raw.clear();
            let i = index;
            index += 1;
            match reader.read_until(b'\n', &mut raw) {
                Ok(0) => return None,
                Ok(_) => {}
                Err(e) => {
                    finished = true;
                    ctx.diagnostic("store_unreadable", None, Some(&path), e.to_string());
                    return None;
                }
            }
            if i < start {
                continue;
            }
            if !needles.is_empty() {
                let lower = raw.to_ascii_lowercase();
                if !needles
                    .iter()
                    .any(|n| n.is_empty() || lower.windows(n.len()).any(|w| w == n))
                {
                    ctx.skipped.set(ctx.skipped.get() + 1);
                    continue;
                }
            }
            match decode(&raw) {
                Ok(v) => return Some((i, v)),
                // A live session's last record can be read mid-write.
                Err(e) if raw.last() != Some(&b'\n') => ctx.diagnostic(
                    "malformed_record",
                    None,
                    Some(&path),
                    format!(
                        "line {}: unterminated final record skipped (file may still be written): {e}",
                        i + 1
                    ),
                ),
                Err(e) => ctx.diagnostic(
                    "malformed_record",
                    None,
                    Some(&path),
                    format!("line {}: {e}", i + 1),
                ),
            }
        }
        None
    }))
}

pub fn tail_lines(ctx: &Context, path: &Path, nbytes: usize) -> Vec<Value> {
    ctx.protect_source(path);
    let data = (|| -> std::io::Result<(u64, Vec<u8>)> {
        let real = locate(path);
        if is_zst(&real) {
            // No seeking inside a compressed stream: keep a sliding window of the end.
            let mut src = Frames::open(&real)?;
            let (mut size, mut tail, mut chunk) = (0u64, Vec::new(), vec![0; 128 * 1024]);
            loop {
                let n = src.read(&mut chunk)?;
                if n == 0 {
                    break;
                }
                size += n as u64;
                tail.extend_from_slice(&chunk[..n]);
                if tail.len() > 2 * nbytes.max(chunk.len()) {
                    tail.drain(..tail.len() - nbytes);
                }
            }
            let keep = tail.len().saturating_sub(nbytes);
            return Ok((size, tail.split_off(keep)));
        }
        let mut f = File::open(real)?;
        let size = f.metadata()?.len();
        f.seek(SeekFrom::Start(size.saturating_sub(nbytes as u64)))?;
        let mut b = Vec::new();
        f.read_to_end(&mut b)?;
        Ok((size, b))
    })();
    let (size, data) = match data {
        Ok(v) => v,
        Err(e) => {
            ctx.diagnostic("store_unreadable", None, Some(path), e.to_string());
            return vec![];
        }
    };
    String::from_utf8_lossy(&data)
        .lines()
        .enumerate()
        .filter_map(|(i, l)| {
            if size > nbytes as u64 && i == 0 {
                return None;
            }
            match decode(l.as_bytes()) {
                Ok(v) => Some(v),
                Err(e) => {
                    ctx.diagnostic(
                        "malformed_record",
                        None,
                        Some(path),
                        format!("tail line {}: {e}", i + 1),
                    );
                    None
                }
            }
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn permissive_and_skipped() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("x");
        std::fs::write(&p, b"broken\n{\"s\":\"A\x01\xff\"}\n{\"s\":\"needle\"}\n").unwrap();
        let ctx = Context::from_env();
        let v = records(&ctx, &p, 0, None).collect::<Vec<_>>();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].0, 1);
        assert_eq!(v[0].1["s"], "A\u{1}\u{fffd}");
        assert_eq!(ctx.diagnostics.borrow().len(), 1);
        let v = records(&ctx, &p, 0, Some(&["needle".into()])).collect::<Vec<_>>();
        assert_eq!(v[0].0, 2);
        assert_eq!(ctx.skipped.get(), 2);
    }
    #[test]
    fn an_unterminated_last_record_is_named_as_such() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("live.jsonl");
        // A writer stopped mid-record, partway into a \u escape.
        std::fs::write(&p, b"{\"a\":1}\nbroken\n{\"s\":\"x\\u12").unwrap();
        let ctx = Context::from_env();
        assert_eq!(records(&ctx, &p, 0, None).count(), 1);
        let d = ctx.diagnostics.borrow();
        assert_eq!(d.len(), 2);
        assert!(d.iter().all(|d| d.code == "malformed_record"));
        assert!(!d[0].message.contains("unterminated"));
        assert!(
            d[1].message
                .starts_with("line 3: unterminated final record")
        );
    }
    fn zst(chunks: &[&[u8]]) -> Vec<u8> {
        use ruzstd::encoding::{CompressionLevel, compress_to_vec};
        chunks
            .iter()
            .flat_map(|c| compress_to_vec(*c, CompressionLevel::Fastest))
            .collect()
    }
    #[test]
    fn a_missing_jsonl_reads_its_zst_sibling_across_frames() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("rollout-x.jsonl");
        std::fs::write(
            t.path().join("rollout-x.jsonl.zst"),
            zst(&[b"{\"a\":1}\n{\"a\":2}\n", b"{\"a\":3}\n"]),
        )
        .unwrap();
        assert!(!p.exists());
        assert!(exists(&p));
        let ctx = Context::from_env();
        let got = records(&ctx, &p, 0, None)
            .map(|(i, v)| (i, v["a"].clone()))
            .collect::<Vec<_>>();
        assert_eq!(got, vec![(0, 1.into()), (1, 2.into()), (2, 3.into())]);
        assert!(ctx.diagnostics.borrow().is_empty());
        assert_eq!(tail_lines(&ctx, &p, 12), vec![serde_json::json!({"a":3})]);
    }
    #[test]
    fn the_tail_of_a_large_zst_is_its_last_lines() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("big.jsonl");
        let body = (0..20_000)
            .map(|i| format!("{{\"i\":{i}}}\n"))
            .collect::<String>();
        let (a, b) = body.as_bytes().split_at(body.len() / 3);
        std::fs::write(t.path().join("big.jsonl.zst"), zst(&[a, b])).unwrap();
        let ctx = Context::from_env();
        assert_eq!(
            tail_lines(&ctx, &p, 26),
            vec![
                serde_json::json!({"i":19998}),
                serde_json::json!({"i":19999})
            ]
        );
        assert_eq!(records(&ctx, &p, 0, None).count(), 20_000);
    }
    #[test]
    fn a_plain_jsonl_wins_over_a_stale_zst() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("live.jsonl");
        std::fs::write(&p, b"{\"a\":\"plain\"}\n").unwrap();
        std::fs::write(
            t.path().join("live.jsonl.zst"),
            zst(&[b"{\"a\":\"old\"}\n"]),
        )
        .unwrap();
        let ctx = Context::from_env();
        assert_eq!(records(&ctx, &p, 0, None).next().unwrap().1["a"], "plain");
    }
    #[test]
    fn a_corrupt_zst_is_reported_not_silent() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("bad.jsonl");
        std::fs::write(t.path().join("bad.jsonl.zst"), b"\x28\xb5\x2f\xfd garbage").unwrap();
        let ctx = Context::from_env();
        assert_eq!(records(&ctx, &p, 0, None).count(), 0);
        assert_eq!(ctx.diagnostics.borrow()[0].code, "store_unreadable");
    }
    #[test]
    fn tail_drops_partial_first_line() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("x");
        std::fs::write(&p, b"{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n").unwrap();
        assert_eq!(
            tail_lines(&Context::from_env(), &p, 12),
            vec![serde_json::json!({"a":3})]
        );
    }
    #[test]
    fn nonfinite_numbers_are_reported_as_malformed_not_coerced() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("numbers.jsonl");
        std::fs::write(&p,b"{\"x\":NaN}\n{\"x\":Infinity}\n{\"x\":-Infinity}\n{\"x\":\"NaN Infinity -Infinity\"}\n").unwrap();
        let ctx = Context::from_env();
        let rows = records(&ctx, &p, 0, None).collect::<Vec<_>>();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, 3);
        assert_eq!(rows[0].1["x"], "NaN Infinity -Infinity");
        let diagnostics = ctx.diagnostics.borrow();
        assert_eq!(diagnostics.len(), 3);
        assert!(diagnostics.iter().all(|d| d.code == "malformed_record"));
    }
}
