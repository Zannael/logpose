# Agent-log fixtures

Synthetic and structurally faithful to the real formats used by Claude Code and Codex CLI. **No real
prompts or messages.** Regenerate with `python3 tests/fixtures/generate_fixtures.py`; never edit the
generated files by hand.

Use `tests/fixtures/claude` as `CLAUDE_CONFIG_DIR` and `tests/fixtures/codex` as `CODEX_HOME`.

## Expected extraction (contract for SESS-001 / SESS-002 / SESS-003)

### Claude Code

| Project (first `cwd`) | Session | Expect |
|---|---|---|
| `/home/user/dev/My Project` | `1111…` | Range 09:00:00.020Z → 09:07:00.500Z on 2026-10-30, despite one out-of-order record. Real prompts in order: "Add a price alert command to the bot", "The alert button overlaps the menu, fix the layout", plus queued "also update the README". Excludes the tool result, the `isMeta` caveat and `/model`. Final message: "Done: added /alert, …". Title "Price alerts" (custom-title beats ai-title). Tokens after dedupe on `(msg_A, req_1)`: input 14, output 320, cache read 10 600, cache create 300. The cost-state agrees. Branch `main`. The `cwd` moves to `…/bot` and back, so the session still belongs to the project root. |
| `/home/user/dev/My Project` | `1111…/subagents/agent-a1b2c3d4` | Sidechain, so not a prompt or activity source. |
| `/home/user/dev/My Project` | `2222…` | Older session (2026-10-20), `entrypoint=cli`, one prompt, no last-prompt or cost-state. |
| `/home/user/dev/other` | `3333…` | One malformed line and one unknown record type, both skipped without error. One prompt "Investigate the flaky test", final message "The test depends on wall-clock time." |

Project-level for `My Project`: last session 2026-10-30T09:07:00.500Z, 2 sessions.

### Codex

| Thread | Files | Expect |
|---|---|---|
| `01a1aaaa-…-0001` | `rollout-…T10-00-00-<id>.jsonl` and continuation `…T15-00-00-<id>_<uuid>.jsonl` | One thread in `/home/user/dev/My Project`, range 10:00:00.000Z → 15:00:31.000Z on 2026-10-30. Prompts: "Wire the WireGuard tunnel to the agent loop", "Now add the handshake retry with backoff" (AGENTS.md, environment_context and developer messages are excluded). Final message "Retry with exponential backoff added.". Title "WireGuard agent loop". Latest rate limit: 34.5% of the weekly window, resets at 1793900000. |
| `01a1aaaa-…-00e1` | `2026/10/31/rollout-…` | Guardian review subagent (`parent_thread_id`), **ignored**. It must not move the project's last activity to 2026-10-31. |
| `01a0bbbb-…-0003` | `archived_sessions/…` | Older 0.153.4 thread with no `thread_source`, in `/home/user/dev/other`. Its prompt appears both as a response_item and as `user_message`, and must be counted **once**. `last_agent_message` is null, so there is no final message. |

### Merged (SESS-003)

`/home/user/dev/My Project` was used by both agents. Its last activity is 2026-10-30T15:00:31.000Z (Codex).
`/home/user/dev/other` was used by both agents. Its last activity is 2026-09-13T16:01:00.000Z (Codex, later than Claude's 2026-09-01).
