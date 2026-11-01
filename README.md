# mini-redis

A Redis-compatible in-memory key-value server in Rust, built on Tokio. It speaks the RESP protocol, so the
real `redis-cli` and Redis client libraries can talk to it.

I built it while learning Rust, after building the same server in Java for the CodeCrafters
"Build Your Own Redis" challenge, to compare the two languages on the same problem.

## Features

- **RESP2 protocol**: parses arrays, bulk strings, integers, simple strings and errors, plus inline commands.
  It handles frames split across TCP reads and many pipelined frames in one read.
- **Commands**: `PING`, `ECHO`, `GET`, `SET key value [EX s | PX ms]`, `DEL`, `EXISTS`, `INCR`, `TTL`,
  `EXPIRE`, `DBSIZE`, `FLUSHALL` / `FLUSHDB`, and `COMMAND` / `CONFIG` stubs for the `redis-cli` handshake.
- **TTL expiry, two ways**:
  - *passive*: an expired key is removed when it is next read
  - *active*: a background Tokio task sweeps expired keys every 100 ms, so keys nobody reads are freed too
- **LRU eviction** with `--max-keys N`: a new key over the limit evicts the least recently used key,
  after first dropping any expired keys.
- **Concurrent clients**: one Tokio task per connection over a shared `Arc<Mutex<…>>` store.
  `INCR` is a read-modify-write under a single lock, so concurrent increments never lose updates.
- **Pipelining**: all replies for the frames in one read go out in a single write.

## Run

```sh
cargo run --release -- --port 6379 --max-keys 10000
# inside Docker: add --host 0.0.0.0
```

```sh
$ redis-cli -p 6379
127.0.0.1:6379> SET session:42 alice EX 10
OK
127.0.0.1:6379> TTL session:42
(integer) 10
127.0.0.1:6379> INCR hits
(integer) 1
```

## Test

```sh
cargo test
```

The acceptance tests start the real server binary and talk to it over TCP. They cover the handshake,
basic commands, error replies, `INCR`, passive and active TTL, LRU eviction, pipelining, frames split
across reads, and concurrent clients.

## Design

| File | Responsibility |
|---|---|
| `src/resp.rs` | RESP parser and encoder. `parse` returns `Ok(None)` until a full frame is buffered. |
| `src/command.rs` | Turns a frame into a typed `Command`, checking arity and arguments. |
| `src/handler.rs` | Runs a `Command` against the store and builds the reply frame. |
| `src/store.rs` | The key space: TTLs, LRU clock, eviction. Every method takes the lock and releases it before returning, so the lock is never held across an `.await`. |
| `src/main.rs` | TCP accept loop, per-connection read/parse/write loop, expiry sweeper. |

LRU uses a logical clock: every read or write stamps the entry with an increasing counter, and the
victim is the entry with the smallest stamp.

## Limitations

These are deliberate simplifications for a learning project:

- **Eviction is O(n)**: finding the LRU victim scans every key. Real Redis samples keys instead; an exact LRU would use
  a hash map plus an intrusive linked list for O(1).
- **One global mutex**: all clients contend on one lock. Sharding the key space would reduce contention.
- **No persistence**: data lives in memory only (no RDB snapshots or AOF).
- **Strings only**: no lists, hashes, sets or sorted sets.
