//! Bounded line framing for the child-process and host-program streams that speak
//! newline-delimited JSON.

use std::collections::VecDeque;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};

/// Why a read produced no line.
#[derive(Debug, PartialEq, Eq)]
pub enum ReadFail {
    Idle,
    TooLong,
    Io,
}

/// Splits a byte stream into lines, however it was chunked: `\n` or `\r\n` ends a line, and a
/// line longer than the cap is an error rather than unbounded memory.
#[derive(Debug)]
pub struct LineBuffer {
    pending: Vec<u8>,
    cap: usize,
}

impl LineBuffer {
    pub fn new(cap: usize) -> Self {
        LineBuffer {
            pending: Vec::new(),
            cap,
        }
    }

    /// Appends `chunk` and returns the lines it completed.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, ReadFail> {
        let mut lines = Vec::new();
        for part in chunk.split_inclusive(|b| *b == b'\n') {
            self.pending.extend_from_slice(part);
            if part.last() == Some(&b'\n') {
                let mut line = std::mem::take(&mut self.pending);
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if line.len() > self.cap {
                    return Err(ReadFail::TooLong);
                }
                if !line.is_empty() {
                    lines.push(line);
                }
            } else if self.pending.len() > self.cap {
                return Err(ReadFail::TooLong);
            }
        }
        Ok(lines)
    }

    /// The final line when the stream ended without a newline.
    pub fn finish(&mut self) -> Option<Vec<u8>> {
        let mut line = std::mem::take(&mut self.pending);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        (!line.is_empty()).then_some(line)
    }
}

/// The longest line accepted.
pub const LINE_CAP: usize = 16 * 1024 * 1024;

/// Reads bounded lines from an async stream.
pub struct LineReader<R> {
    pub inner: R,
    buffer: LineBuffer,
    ready: VecDeque<Vec<u8>>,
    ended: bool,
}

impl<R: AsyncRead + Unpin> LineReader<R> {
    pub fn new(inner: R) -> Self {
        LineReader {
            inner,
            buffer: LineBuffer::new(LINE_CAP),
            ready: VecDeque::new(),
            ended: false,
        }
    }

    /// The next line; `None` at the end of the stream. `idle` bounds the wait for each chunk.
    pub async fn next_line(&mut self, idle: Duration) -> Result<Option<Vec<u8>>, ReadFail> {
        loop {
            if let Some(line) = self.ready.pop_front() {
                return Ok(Some(line));
            }
            if self.ended {
                return Ok(self.buffer.finish());
            }
            let mut chunk = vec![0u8; 16 * 1024];
            match tokio::time::timeout(idle, self.inner.read(&mut chunk)).await {
                Err(_) => return Err(ReadFail::Idle),
                Ok(Err(_)) => return Err(ReadFail::Io),
                Ok(Ok(0)) => self.ended = true,
                Ok(Ok(n)) => self.ready.extend(self.buffer.push(&chunk[..n])?),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_split_however_the_stream_is_chunked() {
        let mut buffer = LineBuffer::new(16);
        assert!(buffer.push(b"ab").unwrap().is_empty());
        assert_eq!(
            buffer.push(b"c\r\nde\n").unwrap(),
            vec![b"abc".to_vec(), b"de".to_vec()]
        );
        assert!(buffer.push(b"tail").unwrap().is_empty());
        assert_eq!(buffer.finish(), Some(b"tail".to_vec()));
    }

    #[test]
    fn a_line_over_the_cap_is_an_error() {
        let mut buffer = LineBuffer::new(3);
        assert_eq!(buffer.push(b"abcd"), Err(ReadFail::TooLong));
    }

    #[test]
    fn lines_split_across_chunks_and_crlf() {
        let mut buffer = LineBuffer::new(64);
        assert!(buffer.push(b"{\"a\":").unwrap().is_empty());
        assert_eq!(
            buffer.push(b"1}\r\n{\"b\":2}\n\n{\"c\"").unwrap(),
            vec![b"{\"a\":1}".to_vec(), b"{\"b\":2}".to_vec()]
        );
        assert_eq!(buffer.finish(), Some(b"{\"c\"".to_vec()));
    }

    #[test]
    fn an_overlong_line_is_refused() {
        let mut buffer = LineBuffer::new(8);
        assert_eq!(buffer.push(b"0123456789"), Err(ReadFail::TooLong));
    }

    #[tokio::test]
    async fn a_reader_returns_lines_then_the_end() {
        let (mut write, read) = tokio::io::duplex(64);
        tokio::io::AsyncWriteExt::write_all(&mut write, b"one\ntwo")
            .await
            .unwrap();
        drop(write);
        let mut reader = LineReader::new(read);
        let idle = Duration::from_secs(1);
        assert_eq!(reader.next_line(idle).await, Ok(Some(b"one".to_vec())));
        assert_eq!(reader.next_line(idle).await, Ok(Some(b"two".to_vec())));
        assert_eq!(reader.next_line(idle).await, Ok(None));
    }
}
