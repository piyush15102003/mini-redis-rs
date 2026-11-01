use crate::command::Command;
use crate::resp::Frame;
use crate::store::Store;

/// Synchronous on purpose: no `.await` in here, so the store's mutex can never be held
/// across a suspension point.
pub fn execute(command: Command, store: &Store) -> Frame {
    match command {
        Command::Ping(None) => Frame::Simple("PONG".to_string()),
        Command::Ping(Some(msg)) | Command::Echo(msg) => Frame::Bulk(Some(msg)),
        Command::Set { key, value, ttl } => {
            store.set(key, value, ttl);
            ok()
        }
        Command::Get(key) => Frame::Bulk(store.get(&key)),
        Command::Del(keys) => Frame::Integer(store.del(&keys)),
        Command::Exists(keys) => Frame::Integer(store.exists(&keys)),
        Command::DbSize => Frame::Integer(store.dbsize()),
        Command::FlushAll => {
            store.flushall();
            ok()
        }
        Command::Incr(key) => match store.incr(&key) {
            Ok(n) => Frame::Integer(n),
            Err(e) => Frame::Error(e),
        },
        Command::Ttl(key) => Frame::Integer(store.ttl(&key)),
        Command::Expire { key, secs } => Frame::Integer(store.expire(&key, secs)),
        Command::CommandDocs => ok(),
        Command::ConfigGet => Frame::Array(Some(vec![])),
    }
}

fn ok() -> Frame {
    Frame::Simple("OK".to_string())
}
