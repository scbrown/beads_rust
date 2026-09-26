//! Synthetic control for aegis-q3q97d: is the import quadratic in fsqlite itself?
//!
//! br's import writes every issue, label and comment row through
//! `Connection::execute_with_params` inside ONE transaction, and the level-3 profile
//! showed per-row cost rising with total transaction work on every table, including
//! one that never exceeds 909 rows. This reproduces that write pattern on a fresh
//! fsqlite file with no br code in the path.
//!
//! Arm A: one transaction (br's shape). Arm B: identical rows, COMMIT every 500.
//! A quadratic, B linear => the cost is per-transaction state in the engine.
//! Both quadratic      => the cost follows table size.
//!
//! Usage: cargo run --release --example fsqlite_control -- <dir> <N>...

use beads_rust::franken_sync::{Connection, SqliteValue};
use std::time::Instant;

fn id(mut x: u64) -> String {
    // deterministic pseudo-random 12-char id (splitmix64), no rand dependency
    const A: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut s = String::with_capacity(12);
    for _ in 0..12 {
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        s.push(A[(z % 36) as usize] as char);
    }
    s
}

fn run(path: &str, n: usize, batch: Option<usize>) -> (f64, f64) {
    let _ = std::fs::remove_file(path);
    for sfx in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{path}{sfx}"));
    }
    let conn = Connection::open(path).expect("open");
    conn.execute("PRAGMA journal_mode=WAL").ok();
    conn.execute(
        "CREATE TABLE issues (id TEXT PRIMARY KEY, title TEXT, description TEXT, status TEXT, priority INTEGER)",
    )
    .expect("create issues");
    conn.execute("CREATE TABLE labels (issue_id TEXT NOT NULL, label TEXT NOT NULL, PRIMARY KEY (issue_id, label))")
        .expect("create labels");
    conn.execute("CREATE INDEX idx_labels_issue ON labels(issue_id)").expect("index");

    let body = "x".repeat(3000); // ~ the export's mean record size
    let (mut t_issue, mut t_label) = (0f64, 0f64);
    conn.execute("BEGIN").expect("begin");
    for i in 0..n {
        let iid = id(i as u64);
        let p = [
            SqliteValue::from(iid.as_str()),
            SqliteValue::from("title"),
            SqliteValue::from(body.as_str()),
            SqliteValue::from("open"),
            SqliteValue::from(2i64),
        ];
        let t = Instant::now();
        conn.execute_with_params(
            "INSERT INTO issues (id, title, description, status, priority) VALUES (?, ?, ?, ?, ?)",
            &p,
        )
        .expect("insert issue");
        t_issue += t.elapsed().as_secs_f64();
        // ~1.45 labels per issue, matching the fixtures (23,233 labels / 16,000 issues)
        let labels = if i % 20 < 9 { 2 } else { 1 };
        for l in 0..labels {
            let lab = format!("label-{}", (i * 7 + l) % 40);
            let lp = [SqliteValue::from(iid.as_str()), SqliteValue::from(lab.as_str())];
            let t = Instant::now();
            conn.execute_with_params("INSERT OR IGNORE INTO labels (issue_id, label) VALUES (?, ?)", &lp)
                .expect("insert label");
            t_label += t.elapsed().as_secs_f64();
        }
        if let Some(b) = batch {
            if (i + 1) % b == 0 {
                conn.execute("COMMIT").expect("commit");
                conn.execute("BEGIN").expect("begin");
            }
        }
    }
    conn.execute("COMMIT").expect("commit");
    (t_issue, t_label)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = &args[1];
    let sizes: Vec<usize> = args[2..].iter().map(|s| s.parse().expect("N")).collect();
    println!("{:>8} {:>10} {:>10} {:>10} {:>10}", "N", "A_issue_s", "A_label_s", "B_issue_s", "B_label_s");
    for n in sizes {
        let (ai, al) = run(&format!("{dir}/a-{n}.db"), n, None);
        let (bi, bl) = run(&format!("{dir}/b-{n}.db"), n, Some(500));
        println!("{n:>8} {ai:>10.3} {al:>10.3} {bi:>10.3} {bl:>10.3}");
    }
}
