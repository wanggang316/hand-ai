//! Strict JSONL framing for RPC stdin/stdout.
//!
//! Frames on `\n` only. Tolerates trailing `\r` (CRLF input) but does not
//! split on U+2028/U+2029 the way `tokio::io::AsyncBufReadExt::lines` would
//! handle some platforms — JSONL strings may legitimately contain those code
//! points and we must not break frames there.
//!
//! Empty frames (a bare `\n` or `\r\n`) are skipped silently, matching the
//! behaviour we want for the RPC pump: blank lines are not commands.

use std::io;

use async_stream::stream;
use futures::Stream;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

/// Errors a JSONL line reader can yield per record.
#[derive(Debug, thiserror::Error)]
pub enum JsonlReadError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid UTF-8 in JSONL frame: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("JSON parse error on line {line_number}: {source}")]
    Parse {
        line_number: usize,
        #[source]
        source: serde_json::Error,
    },
}

/// Read JSONL frames from `reader` and yield parsed `T` values.
///
/// Per-line parse and UTF-8 errors are surfaced as `Err` items but do not
/// stop the stream. The stream ends when the underlying reader returns EOF
/// (any final non-empty buffer without a terminating `\n` is flushed as the
/// last frame). I/O errors terminate the stream after being yielded.
///
/// Empty frames are skipped silently.
pub fn read_jsonl<R, T>(reader: R) -> impl Stream<Item = Result<T, JsonlReadError>> + Send + 'static
where
    R: AsyncBufRead + Send + Unpin + 'static,
    T: DeserializeOwned + Send + 'static,
{
    stream! {
        let mut reader = reader;
        let mut buf: Vec<u8> = Vec::new();
        let mut line_number: usize = 0;

        loop {
            buf.clear();
            // `read_until` includes the delimiter when found; returns 0 only at EOF
            // with no remaining bytes.
            let n = match reader.read_until(b'\n', &mut buf).await {
                Ok(n) => n,
                Err(e) => {
                    yield Err(JsonlReadError::Io(e));
                    return;
                }
            };
            if n == 0 {
                // Clean EOF.
                return;
            }

            // Strip trailing `\n` if present (it is, unless EOF interrupted).
            let had_lf = buf.last() == Some(&b'\n');
            if had_lf {
                buf.pop();
            }
            // Tolerate CRLF: strip a single trailing `\r`.
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }

            line_number += 1;

            if buf.is_empty() {
                if had_lf {
                    // Empty frame — skip silently and continue.
                    continue;
                } else {
                    // EOF with empty trailing buffer — done.
                    return;
                }
            }

            let frame = match std::str::from_utf8(&buf) {
                Ok(s) => s,
                Err(e) => {
                    yield Err(JsonlReadError::Utf8(e));
                    if had_lf {
                        continue;
                    } else {
                        return;
                    }
                }
            };

            match serde_json::from_str::<T>(frame) {
                Ok(value) => yield Ok(value),
                Err(source) => yield Err(JsonlReadError::Parse { line_number, source }),
            }

            if !had_lf {
                // Final partial frame at EOF was just emitted.
                return;
            }
        }
    }
}

/// Serialize `value` and write `<json>\n` to `writer`. Flushes after the LF.
pub async fn write_jsonl<W, T>(writer: &mut W, value: &T) -> io::Result<()>
where
    W: AsyncWrite + Send + Unpin,
    T: Serialize,
{
    let mut bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use serde::{Deserialize, Serialize};
    use tokio::io::{AsyncWriteExt, BufReader, duplex};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct A {
        a: u32,
    }

    async fn collect_all<T: DeserializeOwned + Send + 'static>(
        bytes: Vec<u8>,
    ) -> Vec<Result<T, JsonlReadError>> {
        let reader = BufReader::new(std::io::Cursor::new(bytes));
        read_jsonl::<_, T>(reader).collect().await
    }

    #[tokio::test]
    async fn roundtrip_single_value() {
        let mut sink: Vec<u8> = Vec::new();
        let v = A { a: 7 };
        write_jsonl(&mut sink, &v).await.unwrap();
        assert_eq!(sink.last(), Some(&b'\n'));
        let out: Vec<Result<A, _>> = collect_all(sink).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].as_ref().unwrap(), &v);
    }

    #[tokio::test]
    async fn multiple_frames_in_one_read() {
        let bytes = b"{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n".to_vec();
        let out: Vec<Result<A, _>> = collect_all(bytes).await;
        let values: Vec<u32> = out.into_iter().map(|r| r.unwrap().a).collect();
        assert_eq!(values, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn partial_reads_across_chunks() {
        let (mut tx, rx) = duplex(64);
        let reader = BufReader::new(rx);
        let handle = tokio::spawn(async move {
            read_jsonl::<_, A>(reader).collect::<Vec<_>>().await
        });
        tx.write_all(b"{\"a\":1}\n{\"a\":").await.unwrap();
        tokio::task::yield_now().await;
        tx.write_all(b"2}\n").await.unwrap();
        drop(tx);
        let out = handle.await.unwrap();
        let values: Vec<u32> = out.into_iter().map(|r| r.unwrap().a).collect();
        assert_eq!(values, vec![1, 2]);
    }

    #[tokio::test]
    async fn crlf_tolerated() {
        let bytes = b"{\"a\":1}\r\n{\"a\":2}\r\n".to_vec();
        let out: Vec<Result<A, _>> = collect_all(bytes).await;
        let values: Vec<u32> = out.into_iter().map(|r| r.unwrap().a).collect();
        assert_eq!(values, vec![1, 2]);
    }

    #[tokio::test]
    async fn trailing_partial_frame_at_eof() {
        let bytes = b"{\"a\":1}\n{\"a\":2}".to_vec();
        let out: Vec<Result<A, _>> = collect_all(bytes).await;
        let values: Vec<u32> = out.into_iter().map(|r| r.unwrap().a).collect();
        assert_eq!(values, vec![1, 2]);
    }

    #[tokio::test]
    async fn parse_error_does_not_kill_stream() {
        let bytes = b"{\"a\":1}\nnot json\n{\"a\":3}\n".to_vec();
        let out: Vec<Result<A, _>> = collect_all(bytes).await;
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].as_ref().unwrap().a, 1);
        assert!(matches!(out[1], Err(JsonlReadError::Parse { .. })));
        assert_eq!(out[2].as_ref().unwrap().a, 3);
    }

    #[tokio::test]
    async fn invalid_utf8_yields_utf8_error() {
        let bytes: Vec<u8> = vec![0xff, 0xfe, b'\n', b'{', b'"', b'a', b'"', b':', b'4', b'}', b'\n'];
        let out: Vec<Result<A, _>> = collect_all(bytes).await;
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], Err(JsonlReadError::Utf8(_))));
        assert_eq!(out[1].as_ref().unwrap().a, 4);
    }

    #[tokio::test]
    async fn line_separator_inside_string_does_not_split() {
        #[derive(Debug, Deserialize, Serialize, PartialEq)]
        struct S {
            s: String,
        }
        let bytes = "{\"s\":\"\u{2028}\u{2029}\"}\n".as_bytes().to_vec();
        let out: Vec<Result<S, _>> = collect_all(bytes).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].as_ref().unwrap().s, "\u{2028}\u{2029}");
    }

    #[tokio::test]
    async fn empty_lines_are_skipped() {
        let bytes = b"\n\n{\"a\":1}\n".to_vec();
        let out: Vec<Result<A, _>> = collect_all(bytes).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].as_ref().unwrap().a, 1);
    }

    #[tokio::test]
    async fn write_value_with_line_separator_is_safe() {
        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct S {
            s: String,
        }
        let mut sink: Vec<u8> = Vec::new();
        let v = S { s: "\u{2028}".into() };
        write_jsonl(&mut sink, &v).await.unwrap();
        // Exactly one LF, at the end.
        assert_eq!(sink.iter().filter(|b| **b == b'\n').count(), 1);
        assert_eq!(sink.last(), Some(&b'\n'));
        let out: Vec<Result<S, _>> = collect_all(sink).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].as_ref().unwrap(), &v);
    }
}
