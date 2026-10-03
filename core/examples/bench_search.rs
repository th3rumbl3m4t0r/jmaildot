//! Search speed on a synthetic store: N messages of ~3 KB text.
//! cargo run --release --example bench_search -- /tmp/maildot-bench 100000

use maildot_core::db::Db;
use maildot_core::search::{self, Cond, Field, Query};
use rand::{Rng, SeedableRng};
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::time::Instant;

const WORDS: &[&str] = &[
    "faktura", "invoice", "meeting", "schůzka", "projekt", "deadline", "report", "kůň", "žluťoučký", "server", "backup", "release", "objednávka",
    "order", "payment", "platba", "hello", "ahoj", "tomorrow", "zítra", "budget", "calendar", "draft", "contract", "smlouva", "the", "a", "and",
    "of", "to", "in", "is", "na", "je", "se", "že", "with", "for", "please", "prosím", "thanks", "díky",
];

fn text(rng: &mut impl Rng, words: usize) -> String {
    let mut s = String::with_capacity(words * 7);
    for i in 0..words {
        if i > 0 {
            s.push(if i % 13 == 0 { '\n' } else { ' ' });
        }
        s.push_str(WORDS[rng.gen_range(0..WORDS.len())]);
    }
    s
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = std::path::PathBuf::from(args.get(1).cloned().unwrap_or("/tmp/maildot-bench".into()));
    let n: i64 = args.get(2).and_then(|x| x.parse().ok()).unwrap_or(100_000);
    let path = dir.join("bench.db");
    if !path.exists() {
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&path).unwrap();
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let t = Instant::now();
        let mut c = db.w();
        c.execute("INSERT INTO accounts(name, server, user) VALUES ('bench', 'x', 'x')", []).unwrap();
        let tx = c.transaction().unwrap();
        for i in 1..=n {
            let subject = text(&mut rng, 6);
            let body = text(&mut rng, 450) + if i % 1000 == 0 { " INV-2026-0042 unique-needle" } else { "" };
            tx.execute(
                "INSERT INTO messages(id, account, jid, subject, from_addr, received, keywords, fetched) VALUES (?, 1, ?, ?, ?, ?, '', 1)",
                rusqlite::params![i, format!("j{i}"), subject, format!("user{}@example.com", i % 500), 1_700_000_000 + i * 60],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO texts(id, subject, sender, rcpts, headers, body, attach) VALUES (?,?,?,?,?,?,?)",
                rusqlite::params![i, subject, format!("User {} <user{}@example.com>", i % 500, i % 500), "Me <me@example.com>", format!("From: user{}@example.com\nSubject: {subject}\nX-Mailer: bench", i % 500), body, ""],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        println!("built {n} messages in {:.1} s", t.elapsed().as_secs_f64());
    }
    let size = std::fs::metadata(&path).unwrap().len();
    println!("store: {} MB", size / 1_000_000);
    let queries: Vec<(&str, Cond)> = vec![
        ("plain, any case", Cond::text("unique-needle")),
        ("plain czech, any case", Cond::text("ŽLUŤOUČKÝ KŮŇ")),
        ("case sensitive", Cond { case: true, ..Cond::text("Faktura") }),
        ("regex", Cond { regex: true, ..Cond::text(r"INV-\d{4}-\d{4}") }),
        ("regex, any case, word", Cond { regex: true, word: true, ..Cond::text(r"fak\w+ (na|je)") }),
        ("subject only", Cond { fields: vec![Field::Subject], ..Cond::text("deadline") }),
        ("headers regex", Cond { fields: vec![Field::Headers], regex: true, ..Cond::text(r"(?m)^From: user4\d\d@") }),
    ];
    for threads in [0usize, 1] {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
        println!("--- {} threads", pool.current_num_threads());
        for (name, c) in &queries {
            let q = Query { conds: vec![c.clone()], limit: 5000, ..Default::default() };
            let hits = Mutex::new(0usize);
            let st = pool.install(|| search::search(&path, &q, &AtomicBool::new(false), &|h| *hits.lock().unwrap() += h.len()).unwrap());
            println!(
                "{:<26} {:>7} matches  {:>6} ms  {:>6} MB/s  ({} scanned)",
                name,
                st.matched,
                st.millis,
                if st.millis > 0 { st.bytes as u128 / 1000 / st.millis } else { 0 },
                st.scanned
            );
        }
    }
}
