//! Where the agent CLIs resolve (AGENT-001), and optionally a tiny test call through each. Start it
//! with a short environment to mimic an app launched from the desktop menu:
//! env -i HOME=$HOME PATH=/usr/bin:/bin cargo run -p logpose-core --example agent_lookup -- --call

use logpose_core::config::Config;
use logpose_core::launcher::{find_agent, search_path};
use logpose_core::runner::{self, ClaudeRunner, CodexRunner};
use logpose_core::sessions::Agent;

fn main() {
    let call = std::env::args().any(|a| a == "--call");
    let config = Config::default();
    println!("PATH given: {:?}", std::env::var_os("PATH"));
    println!("search path: {}", search_path().to_string_lossy());
    for agent in [Agent::ClaudeCode, Agent::Codex] {
        let found = find_agent(agent, "");
        println!("{agent:?}: {found:?}");
        let (Some(program), true) = (found, call) else {
            continue;
        };
        let check = match agent {
            Agent::ClaudeCode => runner::check_agent(
                &ClaudeRunner::new(program.clone(), config.agent.model_claude.clone()),
                Some(&program),
            ),
            Agent::Codex => runner::check_agent(
                &CodexRunner::new(program.clone(), Some(config.agent.model_codex.clone())),
                Some(&program),
            ),
        };
        println!(
            "  test call: ok={} tokens={} {:.1}s error={:?}",
            check.ok,
            check.tokens,
            check.millis as f64 / 1000.0,
            check.error
        );
    }
}
