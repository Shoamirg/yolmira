//! Bounded newline framing for both ends of the pipe: a peer that never sends a
//! newline can't make the other side allocate without limit.

use std::io;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

/// Maximum `hello` line (before authentication).
pub const MAX_HELLO_LINE: usize = 4 * 1024;
/// Maximum line after authentication.
pub const MAX_LINE: usize = 1024 * 1024;

/// Read one `\n`-terminated line (without the terminator). `Ok(None)` at EOF;
/// `InvalidData` if the line exceeds `max` bytes or isn't UTF-8.
pub async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> io::Result<Option<String>> {
    let mut buf = Vec::new();
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return if buf.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "line without newline at EOF",
                ))
            };
        }
        let (take, done) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        if buf.len() + take > max + 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("line longer than {max} bytes"),
            ));
        }
        buf.extend_from_slice(&chunk[..take]);
        reader.consume(take);
        if done {
            buf.pop();
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            return String::from_utf8(buf)
                .map(Some)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "line is not UTF-8"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
    }

    #[test]
    fn reads_lines_and_eof() {
        rt().block_on(async {
            let mut r = tokio::io::BufReader::new(&b"one\r\ntwo\n"[..]);
            assert_eq!(read_line(&mut r, 10).await.unwrap().as_deref(), Some("one"));
            assert_eq!(read_line(&mut r, 10).await.unwrap().as_deref(), Some("two"));
            assert_eq!(read_line(&mut r, 10).await.unwrap(), None);
        });
    }

    #[test]
    fn rejects_overlong_line_without_reading_it_all() {
        rt().block_on(async {
            let data = vec![b'x'; 100_000];
            let mut r = tokio::io::BufReader::new(&data[..]);
            let err = read_line(&mut r, 1024).await.unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        });
    }

    #[test]
    fn exact_max_is_allowed() {
        rt().block_on(async {
            let mut line = vec![b'y'; 16];
            line.push(b'\n');
            let mut r = tokio::io::BufReader::new(&line[..]);
            assert_eq!(
                read_line(&mut r, 16).await.unwrap().map(|s| s.len()),
                Some(16)
            );
        });
    }
}
