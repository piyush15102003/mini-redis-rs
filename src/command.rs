use std::time::Duration;

use crate::resp::Frame;

#[derive(Debug, PartialEq)]
pub enum Command {
    Ping(Option<Vec<u8>>),
    Echo(Vec<u8>),
    Set { key: Vec<u8>, value: Vec<u8>, ttl: Option<Duration> },
    Get(Vec<u8>),
    Del(Vec<Vec<u8>>),
    Exists(Vec<Vec<u8>>),
    DbSize,
    FlushAll,
    Incr(Vec<u8>),
    Ttl(Vec<u8>),
    Expire { key: Vec<u8>, secs: i64 },
    /// redis-cli sends `COMMAND DOCS` on connect; any non-array reply makes it skip hints.
    CommandDocs,
    /// redis-cli asks `CONFIG GET save`; reply with an empty array.
    ConfigGet,
}

fn wrong_args(name: &str) -> String {
    format!("ERR wrong number of arguments for '{}' command", name.to_ascii_lowercase())
}

fn syntax_error() -> String {
    "ERR syntax error".to_string()
}

// Arguments stay as raw bytes: keys and values are binary-safe.
fn extract_bytes(frame: Frame) -> Result<Vec<u8>, String> {
    match frame {
        Frame::Bulk(Some(bytes)) => Ok(bytes),
        Frame::Simple(text) => Ok(text.into_bytes()),
        _ => Err("ERR expected a string argument".to_string()),
    }
}

fn parse_int(bytes: &[u8]) -> Result<i64, String> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or_else(|| "ERR value is not an integer or out of range".to_string())
}

impl Command {
    /// Err holds the exact error text to send back as a RESP error frame.
    pub fn from_frame(frame: Frame) -> Result<Command, String> {
        // The incoming frame must be an array.
        let items = match frame {
            Frame::Array(Some(items)) => items,
            _ => return Err("ERR expected an array".to_string()),
        };

        let mut args = items.into_iter().map(extract_bytes).collect::<Result<Vec<_>, _>>()?;
        if args.is_empty() {
            return Err("ERR empty command".to_string());
        }
        let name_bytes = args.remove(0);
        let name = String::from_utf8_lossy(&name_bytes).to_ascii_uppercase();

        // Checks the argument count against an inclusive range.
        let arity = |min: usize, max: usize| -> Result<(), String> {
            if args.len() < min || args.len() > max { Err(wrong_args(&name)) } else { Ok(()) }
        };

        match name.as_str() {
            "PING" => {
                arity(0, 1)?;
                Ok(Command::Ping(args.pop()))
            }
            "ECHO" => {
                arity(1, 1)?;
                Ok(Command::Echo(args.remove(0)))
            }
            "GET" => {
                arity(1, 1)?;
                Ok(Command::Get(args.remove(0)))
            }
            "SET" => {
                // SET key value [EX seconds | PX milliseconds]
                arity(2, 4)?;
                let mut it = args.into_iter();
                let key = it.next().unwrap();
                let value = it.next().unwrap();
                let ttl = match (it.next(), it.next()) {
                    (None, _) => None,
                    (Some(opt), Some(n)) => {
                        let n = parse_int(&n)?;
                        if n <= 0 {
                            return Err("ERR invalid expire time in 'set' command".to_string());
                        }
                        match opt.to_ascii_uppercase().as_slice() {
                            b"EX" => Some(Duration::from_secs(n as u64)),
                            b"PX" => Some(Duration::from_millis(n as u64)),
                            _ => return Err(syntax_error()),
                        }
                    }
                    (Some(_), None) => return Err(syntax_error()),
                };
                Ok(Command::Set { key, value, ttl })
            }
            "DEL" => {
                arity(1, usize::MAX)?;
                Ok(Command::Del(args))
            }
            "EXISTS" => {
                arity(1, usize::MAX)?;
                Ok(Command::Exists(args))
            }
            "DBSIZE" => {
                arity(0, 0)?;
                Ok(Command::DbSize)
            }
            "FLUSHALL" | "FLUSHDB" => Ok(Command::FlushAll),
            "INCR" => {
                arity(1, 1)?;
                Ok(Command::Incr(args.remove(0)))
            }
            "TTL" => {
                arity(1, 1)?;
                Ok(Command::Ttl(args.remove(0)))
            }
            "EXPIRE" => {
                arity(2, 2)?;
                let secs = parse_int(&args[1])?;
                Ok(Command::Expire { key: args.remove(0), secs })
            }
            "COMMAND" => Ok(Command::CommandDocs),
            "CONFIG" => Ok(Command::ConfigGet),
            _ => Err(format!("ERR unknown command '{}'", String::from_utf8_lossy(&name_bytes))),
        }
    }
}
