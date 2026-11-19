// P11 journal append cost: one JSON line appended and made durable, N times.
// A single-file program (rustc only, not part of the Cargo workspace).
//
//   rustc -O -o <scratch>/p11-journal script/manager-v2-proofs/p11-journal.rs
//   <scratch>/p11-journal <file> [n]
//
// Prints p50, p95 and max in microseconds for: write only, write + sync_data
// (on Apple, Rust's sync_data is fcntl F_FULLFSYNC), and write + fsync(2).
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::time::Instant;

extern "C" {
    fn fsync(fd: i32) -> i32;
}

fn pct(mut v: Vec<u128>) -> String {
    v.sort();
    let p = |q: f64| v[((v.len() as f64 - 1.0) * q).round() as usize];
    format!("p50 {} us, p95 {} us, max {} us", p(0.5), p(0.95), v[v.len() - 1])
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = &args[1];
    let n: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(500);
    let line = format!(
        "{{\"t\":\"2026-11-19T14:02:00.000Z\",\"kind\":\"turn.ended\",\"session\":\"s001\",\"cause\":\"Done\",\"summary\":\"{}\"}}\n",
        "x".repeat(300)
    );
    for mode in ["write", "write+sync_data(F_FULLFSYNC)", "write+fsync"] {
        let mut f = OpenOptions::new().create(true).append(true).open(path).unwrap();
        let mut times = Vec::with_capacity(n);
        for _ in 0..n {
            let t = Instant::now();
            f.write_all(line.as_bytes()).unwrap();
            match mode {
                "write+sync_data(F_FULLFSYNC)" => f.sync_data().unwrap(),
                "write+fsync" => unsafe {
                    fsync(f.as_raw_fd());
                },
                _ => {}
            }
            times.push(t.elapsed().as_micros());
        }
        println!("{mode:32} {}", pct(times));
    }
}
