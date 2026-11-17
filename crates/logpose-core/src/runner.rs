//! Agent Runner: the only module that spends tokens (BLUEPRINT §5.5, SYN-003/SYN-004).
//!
//! Each run starts the agent CLI headless with tools disabled, sends the context pack on stdin, and
//! parses the JSON answer with [`crate::synthesis::parse`]. Invocations are pinned in
//! `docs/master.md` §5. Both runners always disable session persistence so Logpose's own runs never
//! show up as user activity (SESS-004).

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::sessions::TokenUsage;
use crate::synthesis::{self, JSON_SCHEMA, Synthesis, SynthesisError};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(180);

/// Instructions given to the agent together with the pack.
pub const INSTRUCTIONS: &str = "You summarize where a software project stands, from the context below only. \
Do not use tools. Answer with exactly one JSON object with the fields description (one short line, at most \
160 characters), state, last_goal, next_steps (1-7 concrete steps, most important first), blockers, and \
confidence (high, medium or low).";

/// Instructions for a batch call (SYN-012): several packs, one answer object keyed by project id.
pub const BATCH_INSTRUCTIONS: &str = "You summarize where several software projects stand, each from its own \
context below only (sections start with `=== Project id: <id> ===`). Do not use tools. Answer with exactly one JSON \
object {\"projects\": [...]} holding one entry per project, with its id and the fields description (one short line, \
at most 160 characters), state, last_goal, next_steps (1-7 concrete steps, most important first), blockers, and \
confidence (high, medium or low). Never mix information between projects.";

/// Tiny synthetic pack for the settings "Test" button (SYN-011): no project data leaves the machine.
pub const TEST_PACK: &str = "Project: logpose-agent-check (/tmp/logpose-agent-check)\n\
Signals: staleness_days=1 bucket=fresh mess=10 label=clean branch=main\n\n\
## Recent commits\n- 1d ago: add a hello-world command\n";

/// Outcome of a test run, shown verbatim in the settings screen.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AgentCheck {
    pub agent: String,
    /// Resolved binary, when found.
    pub program: Option<String>,
    pub ok: bool,
    pub model: Option<String>,
    pub tokens: u64,
    pub millis: u64,
    /// The exact error (spawn failure, CLI flag drift, auth, invalid output, …).
    pub error: Option<String>,
}

/// Run [`TEST_PACK`] through `runner` and report the result or the exact failure (SYN-011).
pub fn check_agent(runner: &dyn Runner, program: Option<&Path>) -> AgentCheck {
    let start = Instant::now();
    let result = runner.run(TEST_PACK);
    let millis = start.elapsed().as_millis() as u64;
    let (ok, model, tokens, error) = match result {
        Ok(out) => (true, out.model, out.tokens.total(), None),
        Err(e) => (false, None, 0, Some(e.to_string())),
    };
    AgentCheck {
        agent: runner.name().to_string(),
        program: program.map(|p| p.display().to_string()),
        ok,
        model,
        tokens,
        millis,
        error,
    }
}

/// The text sent for a batch: each pack under its id header.
pub fn render_batch(packs: &[(&str, &str)]) -> String {
    packs
        .iter()
        .map(|(id, pack)| format!("=== Project id: {id} ===\n{}\n", pack.trim_end()))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("{0}")]
    NotFound(String),
    #[error("failed to run {program}: {source}")]
    Spawn {
        program: PathBuf,
        source: std::io::Error,
    },
    #[error("agent timed out after {0:?}")]
    Timeout(Duration),
    #[error("agent exited with {status}: {detail}")]
    Failed { status: String, detail: String },
    #[error("agent reported an error: {0}")]
    Agent(String),
    #[error("unexpected agent output: {0}")]
    Output(String),
    #[error(transparent)]
    Synthesis(#[from] SynthesisError),
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunOutput {
    pub synthesis: Synthesis,
    pub tokens: TokenUsage,
    pub model: Option<String>,
    pub duration: Duration,
}

/// Result of a batch call: one result per requested id (same order) and the call's total usage.
#[derive(Debug)]
pub struct BatchOutput {
    pub results: Vec<Result<Synthesis, synthesis::SynthesisError>>,
    pub tokens: TokenUsage,
    pub model: Option<String>,
    pub duration: Duration,
}

pub trait Runner: Send + Sync {
    fn name(&self) -> &'static str;
    fn run(&self, pack: &str) -> Result<RunOutput, RunError>;
    /// Synthesize several `(id, pack)` pairs in one call. Runners without batching return an error,
    /// and the caller falls back to single calls.
    fn run_batch(&self, packs: &[(&str, &str)]) -> Result<BatchOutput, RunError> {
        let _ = packs;
        Err(RunError::Output(format!(
            "{} does not support batches",
            self.name()
        )))
    }
}

/// Raw agent answer before synthesis parsing.
struct Answer {
    text: String,
    tokens: TokenUsage,
    model: Option<String>,
    duration: Duration,
}

impl Answer {
    fn single(self) -> Result<RunOutput, RunError> {
        Ok(RunOutput {
            synthesis: synthesis::parse(&self.text)?,
            tokens: self.tokens,
            model: self.model,
            duration: self.duration,
        })
    }

    fn batch(self, packs: &[(&str, &str)]) -> BatchOutput {
        let ids: Vec<&str> = packs.iter().map(|(id, _)| *id).collect();
        BatchOutput {
            results: synthesis::parse_batch(&self.text, &ids),
            tokens: self.tokens,
            model: self.model,
            duration: self.duration,
        }
    }
}

/// Raw result of one subprocess run.
struct Exec {
    stdout: String,
    stderr: String,
    success: bool,
    status: String,
    duration: Duration,
}

fn exec(
    program: &Path,
    args: &[OsString],
    stdin: &str,
    cwd: &Path,
    timeout: Duration,
) -> Result<Exec, RunError> {
    let start = Instant::now();
    // Retry briefly on ETXTBSY ("text file busy"), which happens when the binary was just written.
    let mut attempt = 0;
    let mut child = loop {
        match Command::new(program)
            .args(args)
            .current_dir(cwd)
            .env("PATH", crate::launcher::child_path(program))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => break child,
            Err(e) if e.raw_os_error() == Some(26) && attempt < 10 => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(source) => {
                return Err(RunError::Spawn {
                    program: program.to_owned(),
                    source,
                });
            }
        }
    };
    let mut input = child.stdin.take().expect("piped stdin");
    let payload = stdin.to_string();
    let writer = std::thread::spawn(move || {
        let _ = input.write_all(payload.as_bytes());
    });
    let mut out = child.stdout.take().expect("piped stdout");
    let mut err = child.stderr.take().expect("piped stderr");
    let out_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|source| RunError::Spawn {
            program: program.into(),
            source,
        })? {
            break status;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RunError::Timeout(timeout));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = writer.join();
    Ok(Exec {
        stdout: out_reader.join().unwrap_or_default(),
        stderr: err_reader.join().unwrap_or_default(),
        success: status.success(),
        status: status.to_string(),
        duration: start.elapsed(),
    })
}

fn tail(text: &str, max: usize) -> String {
    let t = text.trim();
    let start = t.char_indices().rev().nth(max).map_or(0, |(i, _)| i);
    t[start..].to_string()
}

/// A neutral working directory so no project `CLAUDE.md`/`AGENTS.md` is picked up.
fn neutral_dir() -> PathBuf {
    std::env::temp_dir()
}

// ---------------------------------------------------------------- Claude Code

pub struct ClaudeRunner {
    pub program: PathBuf,
    pub model: String,
    pub timeout: Duration,
    /// Plan usage from the latest `rate_limit_event` seen by this runner (SYN-009).
    pub last_limits: std::sync::Mutex<Option<ClaudeLimits>>,
}

/// One Claude plan window: share used (0–1) and when it resets.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LimitWindow {
    pub utilization: f64,
    pub resets_at: Option<DateTime<Utc>>,
}

/// Claude Code plan usage, as reported by the `rate_limit_event` of a headless `stream-json` run.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClaudeLimits {
    pub observed_at: DateTime<Utc>,
    pub five_hour: Option<LimitWindow>,
    pub seven_day: Option<LimitWindow>,
}

impl ClaudeLimits {
    /// From a `rate_limit_info` object (`unifiedWindows.{five_hour,seven_day}.{utilization,resetsAt}`).
    pub fn from_info(info: &Value, observed_at: DateTime<Utc>) -> Option<Self> {
        let window = |k: &str| {
            let w = info.pointer(&format!("/unifiedWindows/{k}"))?;
            Some(LimitWindow {
                utilization: w.get("utilization")?.as_f64()?,
                resets_at: w
                    .get("resetsAt")
                    .and_then(Value::as_i64)
                    .and_then(|t| DateTime::from_timestamp(t, 0)),
            })
        };
        let limits = Self {
            observed_at,
            five_hour: window("five_hour"),
            seven_day: window("seven_day"),
        };
        (limits.five_hour.is_some() || limits.seven_day.is_some()).then_some(limits)
    }
}

impl ClaudeRunner {
    pub fn new(program: impl Into<PathBuf>, model: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            model: model.into(),
            timeout: DEFAULT_TIMEOUT,
            last_limits: std::sync::Mutex::new(None),
        }
    }

    pub fn args(&self) -> Vec<OsString> {
        self.args_for(JSON_SCHEMA, INSTRUCTIONS)
    }

    /// The latest plan usage seen by this runner.
    pub fn limits(&self) -> Option<ClaudeLimits> {
        self.last_limits.lock().ok().and_then(|l| l.clone())
    }

    /// A tiny call whose only purpose is to read the plan usage ("Check now" in the picker).
    pub fn probe_limits(&self) -> Result<ClaudeLimits, RunError> {
        self.invoke(
            r#"{"type":"object","properties":{"ok":{"type":"string"}}}"#,
            "Reply with {\"ok\":\"ok\"}.",
            "ok",
        )?;
        self.limits()
            .ok_or_else(|| RunError::Output("Claude Code reported no plan usage".into()))
    }

    fn args_for(&self, schema: &str, instructions: &str) -> Vec<OsString> {
        [
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--model",
            &self.model,
            "--tools",
            "",
            "--no-session-persistence",
            "--setting-sources",
            "",
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--json-schema",
            schema,
            "--system-prompt",
            instructions,
        ]
        .map(OsString::from)
        .to_vec()
    }
}

impl Runner for ClaudeRunner {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    fn run(&self, pack: &str) -> Result<RunOutput, RunError> {
        self.invoke(JSON_SCHEMA, INSTRUCTIONS, pack)?.single()
    }

    fn run_batch(&self, packs: &[(&str, &str)]) -> Result<BatchOutput, RunError> {
        Ok(self
            .invoke(
                &synthesis::batch_schema(),
                BATCH_INSTRUCTIONS,
                &render_batch(packs),
            )?
            .batch(packs))
    }
}

impl ClaudeRunner {
    fn invoke(&self, schema: &str, instructions: &str, input: &str) -> Result<Answer, RunError> {
        let out = exec(
            &self.program,
            &self.args_for(schema, instructions),
            input,
            &neutral_dir(),
            self.timeout,
        )?;
        // stream-json: one event per line. On errors (e.g. "Not logged in") Claude still prints its
        // result event and exits non-zero. A `rate_limit_event` carries the plan usage.
        let mut parsed: Option<Value> = None;
        for line in out.stdout.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            match v.get("type").and_then(Value::as_str) {
                Some("result") => parsed = Some(v),
                Some("rate_limit_event") => {
                    if let Some(l) = v
                        .get("rate_limit_info")
                        .and_then(|i| ClaudeLimits::from_info(i, Utc::now()))
                        && let Ok(mut last) = self.last_limits.lock()
                    {
                        *last = Some(l);
                    }
                }
                _ => {}
            }
        }
        let Some(v) = parsed else {
            return Err(if out.success {
                RunError::Output(tail(&out.stdout, 300))
            } else {
                RunError::Failed {
                    status: out.status,
                    detail: tail(&out.stderr, 300),
                }
            });
        };
        if v.get("is_error").and_then(Value::as_bool) == Some(true) || !out.success {
            let msg = v.get("result").and_then(Value::as_str).unwrap_or("unknown error");
            return Err(RunError::Agent(msg.to_string()));
        }
        let text = match v.get("structured_output").filter(|s| s.is_object()) {
            Some(obj) => obj.to_string(),
            None => v
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        };
        let n = |k: &str| {
            v.pointer(&format!("/usage/{k}"))
                .and_then(Value::as_u64)
                .unwrap_or(0)
        };
        Ok(Answer {
            text,
            tokens: TokenUsage {
                input: n("input_tokens"),
                output: n("output_tokens"),
                cache_read: n("cache_read_input_tokens"),
                cache_creation: n("cache_creation_input_tokens"),
            },
            model: v
                .get("modelUsage")
                .and_then(Value::as_object)
                .and_then(|m| m.keys().next().cloned()),
            duration: out.duration,
        })
    }
}

// ---------------------------------------------------------------- Codex

pub struct CodexRunner {
    pub program: PathBuf,
    /// `None` or `"default"` uses Codex's built-in default model.
    pub model: Option<String>,
    pub timeout: Duration,
}

impl CodexRunner {
    pub fn new(program: impl Into<PathBuf>, model: Option<String>) -> Self {
        Self {
            program: program.into(),
            model,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Config override → `codex` on `PATH` → known per-OS locations (`Platform::codex_fallbacks`).
    pub fn discover(override_path: Option<&str>, model: Option<String>) -> Result<Self, RunError> {
        let configured = override_path.unwrap_or("");
        crate::launcher::find_agent(crate::sessions::Agent::Codex, configured)
            .map(|p| Self::new(p, model))
            .ok_or_else(|| {
                RunError::NotFound(crate::launcher::agent_not_found(
                    crate::sessions::Agent::Codex,
                    configured,
                ))
            })
    }

    pub fn args(&self, schema_file: &Path) -> Vec<OsString> {
        let mut args: Vec<OsString> = [
            "exec",
            "--ephemeral",
            "--skip-git-repo-check",
            "--sandbox",
            "read-only",
            "--ignore-user-config",
            "--ignore-rules",
            "--json",
            "--output-schema",
        ]
        .map(OsString::from)
        .to_vec();
        args.push(schema_file.as_os_str().to_owned());
        if let Some(model) = self.model.as_deref().filter(|m| !m.is_empty() && *m != "default") {
            args.push("--model".into());
            args.push(model.into());
        }
        args
    }
}

impl Runner for CodexRunner {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn run(&self, pack: &str) -> Result<RunOutput, RunError> {
        self.invoke(JSON_SCHEMA, INSTRUCTIONS, pack)?.single()
    }

    fn run_batch(&self, packs: &[(&str, &str)]) -> Result<BatchOutput, RunError> {
        Ok(self
            .invoke(
                &synthesis::batch_schema(),
                BATCH_INSTRUCTIONS,
                &render_batch(packs),
            )?
            .batch(packs))
    }
}

impl CodexRunner {
    fn invoke(&self, schema_json: &str, instructions: &str, input: &str) -> Result<Answer, RunError> {
        let mut schema = tempfile::NamedTempFile::new().map_err(|e| RunError::Output(e.to_string()))?;
        schema
            .write_all(schema_json.as_bytes())
            .map_err(|e| RunError::Output(e.to_string()))?;
        let stdin = format!("{instructions}\n\n{input}");
        let out = exec(
            &self.program,
            &self.args(schema.path()),
            &stdin,
            &neutral_dir(),
            self.timeout,
        )?;
        let mut message = None;
        let mut tokens = TokenUsage::default();
        let mut error = None;
        for line in out.stdout.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            match v.get("type").and_then(Value::as_str) {
                Some("item.completed")
                    if v.pointer("/item/type").and_then(Value::as_str) == Some("agent_message") =>
                {
                    message = v
                        .pointer("/item/text")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                }
                Some("turn.completed") => {
                    let n = |k: &str| {
                        v.pointer(&format!("/usage/{k}"))
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                    };
                    let cached = n("cached_input_tokens");
                    tokens.add(&TokenUsage {
                        input: n("input_tokens").saturating_sub(cached),
                        output: n("output_tokens"),
                        cache_read: cached,
                        cache_creation: n("cache_write_input_tokens"),
                    });
                }
                Some("error" | "turn.failed") => {
                    error = v
                        .get("message")
                        .or_else(|| v.pointer("/error/message"))
                        .and_then(Value::as_str)
                        .map(str::to_string);
                }
                _ => {}
            }
        }
        if let Some(e) = error {
            return Err(RunError::Agent(e));
        }
        if !out.success {
            return Err(RunError::Failed {
                status: out.status,
                detail: tail(&out.stderr, 300),
            });
        }
        let message = message.ok_or_else(|| RunError::Output("no agent_message in Codex output".into()))?;
        Ok(Answer {
            text: message,
            tokens,
            model: self.model.clone().filter(|m| m != "default"),
            duration: out.duration,
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const SYNTH: &str = r#"{"description":"Demo bot","state":"Works.","last_goal":"Fix layout","next_steps":["Ship"],"blockers":[],"confidence":"medium"}"#;

    /// A fake agent: records its args and stdin next to itself, then prints `stdout`.
    fn fake(dir: &Path, stdout: &str, exit: i32) -> PathBuf {
        let body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{d}/args\"\ncat > \"{d}/stdin\"\ncat <<'EOF'\n{stdout}\nEOF\nexit {exit}\n",
            d = dir.display()
        );
        let path = dir.join("agent");
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn recorded(dir: &Path, file: &str) -> String {
        std::fs::read_to_string(dir.join(file)).unwrap()
    }

    #[test]
    fn claude_success_flags_and_usage() {
        let dir = tempfile::tempdir().unwrap();
        let out = format!(
            r#"{{"type":"result","is_error":false,"result":"text","structured_output":{SYNTH},"usage":{{"input_tokens":2,"output_tokens":536,"cache_read_input_tokens":0,"cache_creation_input_tokens":1658}},"modelUsage":{{"claude-haiku-5-5":{{}}}}}}"#
        );
        let r = ClaudeRunner::new(fake(dir.path(), &out, 0), "haiku")
            .run("PACK")
            .unwrap();
        assert_eq!(r.synthesis.top_next_step(), Some("Ship"));
        assert_eq!(r.tokens.total(), 2196);
        assert_eq!(r.model.as_deref(), Some("claude-haiku-5-5"));
        let args = recorded(dir.path(), "args");
        for flag in [
            "--no-session-persistence",
            "--tools",
            "--json-schema",
            "--strict-mcp-config",
        ] {
            assert!(args.lines().any(|l| l == flag), "{flag} missing");
        }
        assert!(
            !args.lines().any(|l| l == "--bare"),
            "--bare breaks subscription auth"
        );
        assert_eq!(recorded(dir.path(), "stdin"), "PACK");
    }

    #[test]
    fn claude_stream_json_reports_plan_usage() {
        let dir = tempfile::tempdir().unwrap();
        let out = format!(
            "{{\"type\":\"system\",\"subtype\":\"init\"}}\n\
{{\"type\":\"rate_limit_event\",\"rate_limit_info\":{{\"status\":\"allowed\",\"unifiedWindows\":{{\"five_hour\":{{\"utilization\":0.43,\"resetsAt\":1793703000}},\"seven_day\":{{\"utilization\":0.05,\"resetsAt\":1794294000}}}}}}}}\n\
{{\"type\":\"result\",\"is_error\":false,\"result\":\"\",\"structured_output\":{SYNTH},\"usage\":{{\"input_tokens\":1,\"output_tokens\":2}}}}"
        );
        let runner = ClaudeRunner::new(fake(dir.path(), &out, 0), "haiku");
        assert!(runner.limits().is_none());
        assert_eq!(runner.run("PACK").unwrap().synthesis.description, "Demo bot");
        let l = runner.limits().unwrap();
        assert_eq!(l.five_hour.unwrap().utilization, 0.43);
        assert_eq!(
            l.seven_day.unwrap().resets_at,
            DateTime::from_timestamp(1_794_294_000, 0)
        );
        let args = recorded(dir.path(), "args");
        assert!(args.contains("stream-json") && args.lines().any(|a| a == "--verbose"));
        // The probe only needs the event.
        assert_eq!(
            runner.probe_limits().unwrap().five_hour.unwrap().utilization,
            0.43
        );
        assert!(ClaudeLimits::from_info(&serde_json::json!({"status": "allowed"}), Utc::now()).is_none());
    }

    #[test]
    fn claude_errors() {
        let dir = tempfile::tempdir().unwrap();
        let not_logged = r#"{"type":"result","is_error":true,"result":"Not logged in · Please run /login"}"#;
        let err = ClaudeRunner::new(fake(dir.path(), not_logged, 1), "haiku")
            .run("x")
            .unwrap_err();
        assert!(
            matches!(&err, RunError::Agent(m) if m.contains("Not logged in")),
            "{err}"
        );
        let bad = r#"{"type":"result","is_error":false,"result":"no json here"}"#;
        assert!(matches!(
            ClaudeRunner::new(fake(dir.path(), bad, 0), "haiku").run("x"),
            Err(RunError::Synthesis(SynthesisError::NoJson))
        ));
        assert!(matches!(
            ClaudeRunner::new(fake(dir.path(), "garbage", 2), "haiku").run("x"),
            Err(RunError::Failed { .. })
        ));
        assert!(matches!(
            ClaudeRunner::new("/nonexistent/claude", "haiku").run("x"),
            Err(RunError::Spawn { .. })
        ));
    }

    #[test]
    fn codex_success_flags_and_usage() {
        let dir = tempfile::tempdir().unwrap();
        let events = format!(
            "{{\"type\":\"thread.started\",\"thread_id\":\"t\"}}\n{{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":{}}}}}\n{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":16579,\"cached_input_tokens\":79,\"cache_write_input_tokens\":0,\"output_tokens\":77}}}}",
            serde_json::to_string(SYNTH).unwrap()
        );
        let runner = CodexRunner::new(fake(dir.path(), &events, 0), Some("default".into()));
        let r = runner.run("PACK").unwrap();
        assert_eq!(r.synthesis.description, "Demo bot");
        assert_eq!(
            (r.tokens.input, r.tokens.cache_read, r.tokens.output),
            (16500, 79, 77)
        );
        let args = recorded(dir.path(), "args");
        for flag in [
            "exec",
            "--ephemeral",
            "--ignore-user-config",
            "--json",
            "--output-schema",
            "read-only",
        ] {
            assert!(args.lines().any(|l| l == flag), "{flag} missing");
        }
        assert!(
            !args.lines().any(|l| l == "--model"),
            "default model must not pass --model"
        );
        assert!(recorded(dir.path(), "stdin").ends_with("\n\nPACK"));
        let with_model = CodexRunner::new("x", Some("gpt-5.5".into())).args(Path::new("/s.json"));
        assert!(
            with_model
                .windows(2)
                .any(|w| w[0] == "--model" && w[1] == "gpt-5.5")
        );
    }

    const BATCH: &str = r#"{"projects":[{"id":"a","description":"A","state":"s","last_goal":"","next_steps":["x"],"blockers":[],"confidence":"low"},{"id":"b","description":"","state":"s","last_goal":"","next_steps":["x"],"blockers":[],"confidence":"low"}]}"#;

    #[test]
    fn batch_calls_send_ids_and_the_batch_schema() {
        let dir = tempfile::tempdir().unwrap();
        let out = format!(
            r#"{{"type":"result","is_error":false,"result":"","structured_output":{BATCH},"usage":{{"input_tokens":10,"output_tokens":200,"cache_read_input_tokens":0,"cache_creation_input_tokens":3000}}}}"#
        );
        let r = ClaudeRunner::new(fake(dir.path(), &out, 0), "haiku")
            .run_batch(&[("a", "PACK A"), ("b", "PACK B")])
            .unwrap();
        assert_eq!(r.results[0].as_ref().unwrap().description, "A");
        assert!(r.results[1].is_err(), "b fails validation");
        assert_eq!(r.tokens.cache_creation, 3000);
        let stdin = recorded(dir.path(), "stdin");
        assert!(
            stdin.contains("=== Project id: a ===\nPACK A")
                && stdin.contains("=== Project id: b ===\nPACK B")
        );
        let args = recorded(dir.path(), "args");
        assert!(args.contains(BATCH_INSTRUCTIONS) && args.contains("\"projects\""));

        let events = format!(
            "{{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":{}}}}}\n{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":17000,\"cached_input_tokens\":0,\"output_tokens\":300}}}}",
            serde_json::to_string(BATCH).unwrap()
        );
        let r = CodexRunner::new(fake(dir.path(), &events, 0), None)
            .run_batch(&[("b", "PACK B"), ("a", "PACK A")])
            .unwrap();
        assert!(
            r.results[0].is_err() && r.results[1].is_ok(),
            "results follow the requested order"
        );
        assert!(recorded(dir.path(), "stdin").starts_with(BATCH_INSTRUCTIONS));
    }

    #[test]
    fn codex_errors_and_discovery() {
        let dir = tempfile::tempdir().unwrap();
        let failed = r#"{"type":"turn.failed","error":{"message":"usage limit reached"}}"#;
        let err = CodexRunner::new(fake(dir.path(), failed, 1), None)
            .run("x")
            .unwrap_err();
        assert!(
            matches!(&err, RunError::Agent(m) if m == "usage limit reached"),
            "{err}"
        );
        let empty = r#"{"type":"turn.completed","usage":{}}"#;
        assert!(matches!(
            CodexRunner::new(fake(dir.path(), empty, 0), None).run("x"),
            Err(RunError::Output(_))
        ));
        let exe = fake(dir.path(), "", 0);
        let r = CodexRunner::discover(Some(exe.to_str().unwrap()), None).unwrap();
        assert_eq!(r.program, exe);
        assert!(matches!(
            CodexRunner::discover(Some("/nonexistent/codex"), None),
            Err(RunError::NotFound(_))
        ));
    }

    #[test]
    fn timeout_kills_the_agent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("slow");
        std::fs::write(&path, "#!/bin/sh\nsleep 5\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut r = ClaudeRunner::new(path, "haiku");
        r.timeout = Duration::from_millis(200);
        assert!(matches!(r.run("x"), Err(RunError::Timeout(_))));
    }

    #[test]
    fn check_agent_reports_success_or_the_exact_failure() {
        let dir = tempfile::tempdir().unwrap();
        let out = format!(
            r#"{{"type":"result","is_error":false,"result":"","structured_output":{SYNTH},"usage":{{"input_tokens":3,"output_tokens":40,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}},"modelUsage":{{"claude-haiku-5-5":{{}}}}}}"#
        );
        let exe = fake(dir.path(), &out, 0);
        let c = check_agent(&ClaudeRunner::new(&exe, "haiku"), Some(&exe));
        assert!(c.ok && c.error.is_none());
        assert_eq!(
            (c.agent.as_str(), c.model.as_deref(), c.tokens),
            ("claude-code", Some("claude-haiku-5-5"), 43)
        );
        assert!(recorded(dir.path(), "stdin").contains("logpose-agent-check"));

        // CLI flag drift: the agent rejects an option and exits non-zero.
        let drift = fake(dir.path(), "", 2);
        std::fs::write(
            &drift,
            "#!/bin/sh\necho \"error: unexpected argument '--ephemeral' found\" >&2\nexit 2\n",
        )
        .unwrap();
        let c = check_agent(&CodexRunner::new(&drift, None), Some(&drift));
        assert!(!c.ok);
        let err = c.error.unwrap();
        assert!(err.contains("unexpected argument '--ephemeral'"), "{err}");

        let c = check_agent(&ClaudeRunner::new("/nonexistent/claude", "haiku"), None);
        assert!(!c.ok && c.error.unwrap().contains("/nonexistent/claude"));
    }

    /// Live calls; they spend a few tokens. Run with: LOGPOSE_LIVE_AGENTS=1 cargo test -p logpose-core live_ -- --nocapture
    #[test]
    fn live_agents() {
        if std::env::var_os("LOGPOSE_LIVE_AGENTS").is_none() {
            return;
        }
        let pack = "Project: demo-bot\nLast commits: \"add /alert command\" (2d ago)\nLast user prompt: \"fix the alert layout\"";
        let models = crate::config::AgentConfig::default();
        let claude_runner = ClaudeRunner::new("claude", models.model_claude.clone());
        let claude = claude_runner.run(pack).unwrap();
        println!("claude limits: {:?}", claude_runner.limits());
        println!(
            "claude: {:?} {:?} {:?}",
            claude.model, claude.tokens, claude.duration
        );
        let codex = CodexRunner::discover(None, Some(models.model_codex.clone()))
            .unwrap()
            .run(pack)
            .unwrap();
        println!("codex: {:?} {:?} {:?}", codex.model, codex.tokens, codex.duration);
    }

    /// SYN-012 measurement: tokens per project for 1 single call vs a batch of 4 synthetic packs
    /// (~600 estimated tokens each, like the real median). Same opt-in as `live_agents`.
    #[test]
    fn live_batch() {
        if std::env::var_os("LOGPOSE_LIVE_AGENTS").is_none() {
            return;
        }
        let pack = |n: usize| {
            let mut p = format!(
                "Project: demo-{n} (/home/user/dev/demo-{n})\nSignals: staleness_days={n} bucket=cooling mess=30 label=tidy-up branch=main\n\n## Recent commits\n"
            );
            for c in 0..10 {
                p.push_str(&format!(
                    "- {c}d ago: refactor module {c} of demo-{n} and update its tests\n"
                ));
            }
            p.push_str("\n## Uncommitted changes (files)\n");
            for f in 0..15 {
                p.push_str(&format!("- src/feature_{f}/handler_{n}.rs\n"));
            }
            p.push_str(&format!("\n## Last user prompts to coding agents (oldest first)\n- add the export command to demo-{n}\n- fix the failing export test\n"));
            p
        };
        let packs: Vec<String> = (1..=4).map(pack).collect();
        let ids = ["a", "b", "c", "d"];
        let pairs: Vec<(&str, &str)> = ids.iter().zip(&packs).map(|(i, p)| (*i, p.as_str())).collect();
        let est: usize = packs
            .iter()
            .map(|p| crate::packer::estimate_tokens(p))
            .sum::<usize>()
            / packs.len();
        println!("pack size ≈ {est} estimated tokens");
        let report = |name: &str, single: &TokenUsage, batch: &BatchOutput| {
            let ok = batch.results.iter().filter(|r| r.is_ok()).count();
            println!(
                "{name}: single total {} (in {} / cache_r {} / cache_w {} / out {}); batch of 4 total {} → {} per project, {ok}/4 valid, {:?}",
                single.total(),
                single.input,
                single.cache_read,
                single.cache_creation,
                single.output,
                batch.tokens.total(),
                batch.tokens.total() / 4,
                batch.duration
            );
        };
        let models = crate::config::AgentConfig::default();
        let claude = ClaudeRunner::new("claude", models.model_claude.clone());
        let one = claude.run(&packs[0]).unwrap();
        report("claude", &one.tokens, &claude.run_batch(&pairs).unwrap());
        let codex = CodexRunner::discover(None, Some(models.model_codex.clone())).unwrap();
        let one = codex.run(&packs[0]).unwrap();
        report("codex", &one.tokens, &codex.run_batch(&pairs).unwrap());
    }
}
