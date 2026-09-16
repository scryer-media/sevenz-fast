//! Decode-throughput harness for the codec swap.
//!
//! ```text
//! decode-bench [--runs N] [--no-oracle] <archive.7z> ...
//! ```
//!
//! For each archive it times, in one session on one machine:
//!
//! - `7zz t -mmt=1` (the C reference, single-threaded),
//! - upstream `sevenz-rust2` 0.22.2, the release weaver ships today,
//! - this fork,
//!
//! extracting every entry to a discard sink, and reports the median wall time
//! and the MiB/s of *uncompressed* output. The numbers this produced are in
//! `docs/benchmarking.md`; the acceptance gate for the swap is "faster than
//! upstream, and within 5% of what `lzma-bench` gets on the bare LZMA2
//! stream", which is what the ratios printed here are read against.
//!
//! Not a criterion benchmark on purpose: the inputs are ~900 MiB fixtures that
//! live outside the repository, the run takes minutes, and it is a thing an
//! operator runs deliberately rather than something CI measures.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const HELP: &str = "\
usage: decode-bench [--runs N] [--no-oracle] <archive.7z> ...

  --runs N     repetitions per decoder (default 3); the median is reported
  --no-oracle  skip `7zz t -mmt=1`
  --only NAME  run only `7zz`, `upstream` or `fork`";

fn main() {
    let mut runs = 3usize;
    let mut oracle = true;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut only = String::new();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--runs" => {
                runs = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| fail("--runs needs a number"));
            }
            "--no-oracle" => oracle = false,
            "--only" => {
                only = args.next().unwrap_or_else(|| fail("--only needs a name"));
            }
            "-h" | "--help" => {
                println!("{HELP}");
                return;
            }
            other if other.starts_with('-') => fail(&format!("unknown option {other}")),
            other => files.push(PathBuf::from(other)),
        }
    }

    if files.is_empty() {
        println!("{HELP}");
        std::process::exit(2);
    }

    println!("decode-bench, {runs} run(s) per decoder, median reported");

    for file in &files {
        bench_one(file, runs, oracle, &only);
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("decode-bench: {msg}");
    std::process::exit(2);
}

/// A sink that counts bytes and keeps the CRC-32 of everything written, so two
/// decoders can be shown to have produced the same output rather than assumed
/// to have.
struct Sink {
    bytes: u64,
    digest: u64,
    /// Bytes left over from the last update, so the digest depends on the byte
    /// stream and not on where each decoder happened to end its reads.
    tail: [u8; 8],
    tail_len: usize,
}

impl Default for Sink {
    fn default() -> Self {
        Self {
            bytes: 0,
            digest: 0xcbf2_9ce4_8422_2325,
            tail: [0; 8],
            tail_len: 0,
        }
    }
}

const DIGEST_K: u64 = 0x100_0000_01b3;

impl Sink {
    fn update(&mut self, mut buf: &[u8]) {
        self.bytes += buf.len() as u64;

        if self.tail_len > 0 {
            let take = (8 - self.tail_len).min(buf.len());
            self.tail[self.tail_len..self.tail_len + take].copy_from_slice(&buf[..take]);
            self.tail_len += take;
            buf = &buf[take..];
            if self.tail_len < 8 {
                return;
            }
            self.mix(u64::from_le_bytes(self.tail));
            self.tail_len = 0;
        }

        let mut chunks = buf.chunks_exact(8);
        for chunk in &mut chunks {
            self.mix(u64::from_le_bytes(chunk.try_into().expect("8 bytes")));
        }
        let rest = chunks.remainder();
        self.tail[..rest.len()].copy_from_slice(rest);
        self.tail_len = rest.len();
    }

    /// An order-sensitive 64-bit digest, not a CRC: its only job is to show
    /// that two decoders produced the same bytes, and it has to cost far less
    /// than the decode it is measuring. (A byte-at-a-time CRC-32 here cost a
    /// quarter of the fork's measured time and made the fork look 30% slower
    /// than it is.)
    #[inline]
    fn mix(&mut self, word: u64) {
        self.digest = (self.digest ^ word).wrapping_mul(DIGEST_K).rotate_left(23);
    }

    /// Folds in whatever is left in the carry buffer. Call once per stream.
    fn finish(&mut self) -> u64 {
        for index in 0..self.tail_len {
            self.digest = (self.digest ^ u64::from(self.tail[index])).wrapping_mul(DIGEST_K);
        }
        self.tail_len = 0;
        self.digest
    }
}

fn drain<R: Read + ?Sized>(reader: &mut R, sink: &mut Sink, buf: &mut [u8]) -> std::io::Result<()> {
    loop {
        let read = reader.read(buf)?;
        if read == 0 {
            return Ok(());
        }
        sink.update(&buf[..read]);
    }
}

fn extract_fork(path: &Path) -> Sink {
    let file = std::fs::File::open(path).expect("open archive");
    let mut reader = sevenz_fast::ArchiveReader::new(file, sevenz_fast::Password::empty())
        .expect("fork: read header");
    let mut sink = Sink::default();
    let mut buf = vec![0u8; 1 << 20];
    reader
        .for_each_entries(|entry, rd| {
            if !entry.is_directory() {
                drain(rd, &mut sink, &mut buf)?;
            }
            Ok(true)
        })
        .expect("fork: extract");
    sink.finish();
    sink
}

fn extract_upstream(path: &Path) -> Sink {
    let file = std::fs::File::open(path).expect("open archive");
    let mut reader = sevenz_rust2::ArchiveReader::new(file, sevenz_rust2::Password::empty())
        .expect("upstream: read header");
    let mut sink = Sink::default();
    let mut buf = vec![0u8; 1 << 20];
    reader
        .for_each_entries(|entry, rd| {
            if !entry.is_directory() {
                drain(rd, &mut sink, &mut buf)?;
            }
            Ok(true)
        })
        .expect("upstream: extract");
    sink.finish();
    sink
}

/// `7zz t -mmt=1`: a full decode plus CRC check, with no output file, which is
/// the closest the CLI offers to what the two library paths above do.
fn oracle_7zz(path: &Path) -> Option<Duration> {
    let start = Instant::now();
    let status = Command::new("7zz")
        .args(["t", "-mmt=1", "-bso0", "-bsp0"])
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    status.success().then(|| start.elapsed())
}

fn median(mut times: Vec<Duration>) -> Duration {
    times.sort();
    times[times.len() / 2]
}

fn bench_one(path: &Path, runs: usize, oracle: bool, only: &str) {
    let wanted = |name: &str| only.is_empty() || only == name;
    let compressed = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    println!(
        "\n{} ({:.1} MiB packed)",
        path.display(),
        compressed as f64 / (1024.0 * 1024.0)
    );

    let mut rows: Vec<(&str, Duration, Option<Sink>)> = Vec::new();

    if oracle && wanted("7zz") {
        let mut times = Vec::new();
        for _ in 0..runs {
            print!("  7zz …");
            let _ = std::io::stdout().flush();
            match oracle_7zz(path) {
                Some(elapsed) => times.push(elapsed),
                None => {
                    println!("\r  7zz: unavailable          ");
                    times.clear();
                    break;
                }
            }
            print!("\r");
        }
        if !times.is_empty() {
            rows.push(("7zz t -mmt=1", median(times), None));
        }
    }

    for (name, run) in [
        ("sevenz-rust2 0.22.2", extract_upstream as fn(&Path) -> Sink),
        ("sevenz-fast", extract_fork as fn(&Path) -> Sink),
    ] {
        if !wanted(if name == "sevenz-fast" {
            "fork"
        } else {
            "upstream"
        }) {
            continue;
        }
        let mut times = Vec::new();
        let mut last = None;
        for _ in 0..runs {
            print!("  {name} …");
            let _ = std::io::stdout().flush();
            let start = Instant::now();
            let sink = run(path);
            times.push(start.elapsed());
            last = Some(sink);
            print!("\r");
        }
        rows.push((name, median(times), last));
    }

    let unpacked = rows
        .iter()
        .find_map(|(_, _, sink)| sink.as_ref().map(|s| s.bytes))
        .unwrap_or(0);
    println!("  {:.1} MiB unpacked", unpacked as f64 / (1024.0 * 1024.0));

    let baseline = rows
        .iter()
        .find(|(name, _, _)| *name == "sevenz-rust2 0.22.2")
        .map(|(_, time, _)| *time);

    println!(
        "  {:<22} {:>9}  {:>11}  {:>8}  digest",
        "decoder", "median", "MiB/s", "vs 0.22.2"
    );
    for (name, time, sink) in &rows {
        let secs = time.as_secs_f64();
        let throughput = if unpacked > 0 && secs > 0.0 {
            format!("{:.1}", unpacked as f64 / (1024.0 * 1024.0) / secs)
        } else {
            "-".to_string()
        };
        let ratio = match baseline {
            Some(base) => format!("{:.2}x", base.as_secs_f64() / secs),
            None => "-".to_string(),
        };
        let crc = sink
            .as_ref()
            .map_or_else(|| "-".to_string(), |s| format!("{:016x}", s.digest));
        println!("  {name:<22} {secs:>8.3}s  {throughput:>11}  {ratio:>8}  {crc}");
    }

    let digests: Vec<u64> = rows
        .iter()
        .filter_map(|(_, _, s)| s.as_ref().map(|s| s.digest))
        .collect();
    if digests.windows(2).any(|w| w[0] != w[1]) {
        println!("  WARNING: the decoders disagree on the output bytes");
    }
}
