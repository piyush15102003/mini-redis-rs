use bytes::{Buf, BytesMut};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Simple(String),
    Error(String),
    Integer(i64),
    Bulk(Option<Vec<u8>>),
    Array(Option<Vec<Frame>>),
}

/// Try to parse ONE complete frame from the start of `buf`.
/// Ok(Some(frame)) = parsed it AND removed its bytes from buf.
/// Ok(None)        = not enough bytes yet — read more from the socket and call again.
/// Err(..)         = the bytes are not valid RESP.
pub fn parse(buf: &mut BytesMut) -> Result<Option<Frame>, String> {
    // Parse against a read-only slice with a cursor, and only consume bytes once a whole
    // frame is known to be present. No cloning of the buffer for nested arrays.
    match parse_at(&buf[..], 0)? {
        Some((frame, end)) => {
            buf.advance(end);
            Ok(Some(frame))
        }
        None => Ok(None),
    }
}

/// Finds the next "\r\n" at or after `start`; returns the index of the '\r'.
fn find_crlf(data: &[u8], start: usize) -> Option<usize> {
    data[start..].windows(2).position(|w| w == b"\r\n").map(|p| start + p)
}

/// Reads the line starting at `start` (after the type byte) and parses it as an i64.
/// Returns the number and the index just past its "\r\n".
fn parse_number(data: &[u8], start: usize) -> Result<Option<(i64, usize)>, String> {
    let Some(end) = find_crlf(data, start) else { return Ok(None) };
    let n = std::str::from_utf8(&data[start..end])
        .map_err(|e| e.to_string())?
        .parse::<i64>()
        .map_err(|e| e.to_string())?;
    Ok(Some((n, end + 2)))
}

/// Parses one frame beginning at `pos`. Returns the frame and the index just past it.
fn parse_at(data: &[u8], pos: usize) -> Result<Option<(Frame, usize)>, String> {
    let Some(&first) = data.get(pos) else { return Ok(None) };

    match first {
        b'+' | b'-' => {
            let Some(end) = find_crlf(data, pos + 1) else { return Ok(None) };
            let text = String::from_utf8(data[pos + 1..end].to_vec()).map_err(|e| e.to_string())?;
            let frame = if first == b'+' { Frame::Simple(text) } else { Frame::Error(text) };
            Ok(Some((frame, end + 2)))
        }

        b':' => match parse_number(data, pos + 1)? {
            Some((n, next)) => Ok(Some((Frame::Integer(n), next))),
            None => Ok(None),
        },

        b'$' => {
            let Some((len, data_start)) = parse_number(data, pos + 1)? else { return Ok(None) };
            if len == -1 {
                return Ok(Some((Frame::Bulk(None), data_start)));
            }
            if len < -1 {
                return Err("invalid bulk length".to_string());
            }
            // $5\r\nhello\r\n — need header + payload + trailing CRLF.
            let data_end = data_start + len as usize;
            if data.len() < data_end + 2 {
                return Ok(None);
            }
            if &data[data_end..data_end + 2] != b"\r\n" {
                return Err("invalid bulk string: missing CRLF".to_string());
            }
            Ok(Some((Frame::Bulk(Some(data[data_start..data_end].to_vec())), data_end + 2)))
        }

        b'*' => {
            let Some((count, mut next)) = parse_number(data, pos + 1)? else { return Ok(None) };
            if count == -1 {
                return Ok(Some((Frame::Array(None), next)));
            }
            if count < -1 {
                return Err("invalid array length".to_string());
            }
            let mut frames = Vec::with_capacity(count as usize);
            for _ in 0..count {
                match parse_at(data, next)? {
                    Some((frame, after)) => {
                        frames.push(frame);
                        next = after;
                    }
                    // One of the elements is incomplete; consume nothing.
                    None => return Ok(None),
                }
            }
            Ok(Some((Frame::Array(Some(frames)), next)))
        }

        // Inline command, as redis-cli sends interactively: "PING\r\n".
        // Split on whitespace and present it as an array of bulk strings.
        _ => {
            let Some(end) = find_crlf(data, pos) else { return Ok(None) };
            let parts = data[pos..end]
                .split(|b| b.is_ascii_whitespace())
                .filter(|p| !p.is_empty())
                .map(|p| Frame::Bulk(Some(p.to_vec())))
                .collect();
            Ok(Some((Frame::Array(Some(parts)), end + 2)))
        }
    }
}

pub fn encode(frame: &Frame, out: &mut Vec<u8>) {
    match frame {
        // Simple String: +OK\r\n
        Frame::Simple(value) => {
            out.extend_from_slice(b"+");
            out.extend_from_slice(value.as_bytes());
            out.extend_from_slice(b"\r\n");
        }

        // Error: -ERR unknown command\r\n
        Frame::Error(value) => {
            out.extend_from_slice(b"-");
            out.extend_from_slice(value.as_bytes());
            out.extend_from_slice(b"\r\n");
        }

        // Integer: :42\r\n
        Frame::Integer(value) => {
            out.extend_from_slice(b":");
            out.extend_from_slice(value.to_string().as_bytes());
            out.extend_from_slice(b"\r\n");
        }

        // Bulk String: $5\r\nhello\r\n
        Frame::Bulk(Some(value)) => {
            out.extend_from_slice(b"$");
            out.extend_from_slice(value.len().to_string().as_bytes());
            out.extend_from_slice(b"\r\n");
            out.extend_from_slice(value);
            out.extend_from_slice(b"\r\n");
        }

        // Null Bulk String: $-1\r\n
        Frame::Bulk(None) => {
            out.extend_from_slice(b"$-1\r\n");
        }

        // Array: *2\r\n<frame1><frame2>
        Frame::Array(Some(frames)) => {
            out.extend_from_slice(b"*");
            out.extend_from_slice(frames.len().to_string().as_bytes());
            out.extend_from_slice(b"\r\n");

            for frame in frames {
                encode(frame, out);
            }
        }

        // Null Array: *-1\r\n
        Frame::Array(None) => {
            out.extend_from_slice(b"*-1\r\n");
        }
    }
}
