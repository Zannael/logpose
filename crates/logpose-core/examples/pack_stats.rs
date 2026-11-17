//! Context-pack sizes on the real config (counts only, never pack text).
//! Run: cargo run -p logpose-core --release --example pack_stats

use chrono::Utc;
use logpose_core::config::{self, Config};
use logpose_core::packer::{self, PackOptions};
use logpose_core::scan::{self, LogSources};

fn main() {
    let config = Config::load(&config::default_path().expect("config path")).expect("config");
    let report = scan::scan(&config, &LogSources::defaults(), Utc::now()).expect("scan");
    let opts = PackOptions {
        budget_tokens: config.agent.context_budget_tokens,
        ..PackOptions::default()
    };
    let mut sizes: Vec<usize> = Vec::new();
    let mut truncated = 0;
    for p in &report.projects {
        let pack = packer::pack(p, &opts, Utc::now());
        truncated += usize::from(!pack.truncated.is_empty());
        sizes.push(pack.estimated_tokens);
    }
    sizes.sort();
    let total: usize = sizes.iter().sum();
    println!(
        "{} packs, budget {}: min {} median {} max {} total {} est. tokens; {} needed truncation",
        sizes.len(),
        opts.budget_tokens,
        sizes.first().unwrap_or(&0),
        sizes.get(sizes.len() / 2).unwrap_or(&0),
        sizes.last().unwrap_or(&0),
        total,
        truncated
    );
}
