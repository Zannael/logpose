//! DEL-004 on a copy of a database: prune it with the app's rule and compare the Stats staleness
//! series and the file size before and after. Run on a COPY:
//! cargo run -p logpose-core --release --example prune_check -- <copy.sqlite>
use chrono::{Duration, Local, Utc};
use logpose_core::{stats, store::Store};

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("db copy"));
    let size = || std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let before_size = size();
    let mut store = Store::open(&path).expect("store");
    let series = |s: &Store| {
        stats::bucket_series(
            &s.snapshots_since(Utc::now() - Duration::days(3660)).unwrap(),
            &Local,
        )
    };
    let before = series(&store);
    let gone = |p: &std::path::Path| !p.exists() && p.parent().is_some_and(|d| d.is_dir());
    let r = store.prune(Utc::now(), &Local, &gone).expect("prune");
    let after = series(&store);
    drop(store);
    println!(
        "{r:?}; series identical: {}; {} days; size {} → {} bytes",
        before == after,
        after.len(),
        before_size,
        size()
    );
}
