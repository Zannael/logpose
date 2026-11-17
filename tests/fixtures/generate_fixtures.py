#!/usr/bin/env python3
"""Generate the synthetic agent-log fixtures under tests/fixtures/{claude,codex}.

All content is invented. The structure mirrors what SPIKE-001/SPIKE-002 observed in real logs
(Claude Code 2.1.209–2.1.296, Codex CLI 0.153.4–0.162.0-alpha.2); see docs/master.md §4.
Re-run after changing: python3 tests/fixtures/generate_fixtures.py
"""

import json
import shutil
from pathlib import Path

HERE = Path(__file__).resolve().parent
CLAUDE = HERE / "claude"
CODEX = HERE / "codex"

PROJECT = "/home/user/dev/My Project"
OTHER = "/home/user/dev/other"
S1 = "11111111-1111-4111-8111-111111111111"
S2 = "22222222-2222-4222-8222-222222222222"
S3 = "33333333-3333-4333-8333-333333333333"


def write_jsonl(path: Path, records: list, raw_lines: dict | None = None) -> None:
    """Write records; raw_lines maps index -> literal line inserted before that record."""
    path.parent.mkdir(parents=True, exist_ok=True)
    lines = []
    for i, rec in enumerate(records):
        if raw_lines and i in raw_lines:
            lines.append(raw_lines[i])
        lines.append(json.dumps(rec, separators=(",", ":")))
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


# ---------------------------------------------------------------- Claude Code

def claude_base(session, cwd, ts, uuid, parent, branch="main", version="2.1.284", **extra):
    rec = {"parentUuid": parent, "isSidechain": False, "userType": "external", "entrypoint": "claude-desktop",
           "cwd": cwd, "sessionId": session, "version": version, "gitBranch": branch, "uuid": uuid, "timestamp": ts}
    rec.update(extra)
    return rec


def usage(inp, out, cread=0, ccreate=0):
    return {"input_tokens": inp, "cache_creation_input_tokens": ccreate, "cache_read_input_tokens": cread,
            "output_tokens": out, "service_tier": "standard",
            "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": ccreate},
            "inference_geo": "", "speed": "standard"}


def assistant(session, cwd, ts, uuid, parent, msg_id, req, block, stop, u, model="claude-opus-5-5"):
    return claude_base(session, cwd, ts, uuid, parent, type="assistant", requestId=req, message={
        "model": model, "id": msg_id, "type": "message", "role": "assistant", "content": [block],
        "stop_reason": stop, "stop_sequence": None, "stop_details": None, "usage": u})


def user(session, cwd, ts, uuid, parent, content, **extra):
    return claude_base(session, cwd, ts, uuid, parent, type="user", promptId="p-" + uuid,
                       message={"role": "user", "content": content}, **extra)


def build_claude() -> None:
    shutil.rmtree(CLAUDE, ignore_errors=True)
    pdir = CLAUDE / "projects" / "-home-user-dev-My-Project"  # '/' and ' ' both become '-'
    sub = f"{PROJECT}/bot"
    u1 = usage(10, 200, cread=5000, ccreate=300)
    u2 = usage(4, 120, cread=5600)
    human = {"origin": {"kind": "human"}, "promptSource": "sdk"}
    s1 = [
        {"type": "queue-operation", "operation": "enqueue", "timestamp": "2026-10-30T09:00:00.000Z",
         "sessionId": S1, "content": "Add a price alert command to the bot"},
        {"type": "queue-operation", "operation": "dequeue", "timestamp": "2026-10-30T09:00:00.010Z", "sessionId": S1},
        user(S1, PROJECT, "2026-10-30T09:00:00.020Z", "u-01", None, "Add a price alert command to the bot", **human),
        claude_base(S1, PROJECT, "2026-10-30T09:00:00.030Z", "a-01", "u-01", type="attachment",
                    attachment={"type": "total_tokens_reminder", "text": "synthetic"}),
        assistant(S1, PROJECT, "2026-10-30T09:00:03.500Z", "m-01", "a-01", "msg_A", "req_1",
                  {"type": "thinking", "thinking": "", "signature": "sig"}, "tool_use", u1),
        # Same message id/request id as above, identical usage; timestamp slightly earlier (stream jitter).
        assistant(S1, PROJECT, "2026-10-30T09:00:03.200Z", "m-02", "m-01", "msg_A", "req_1",
                  {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "ls"}}, "tool_use", u1),
        user(S1, sub, "2026-10-30T09:00:04.000Z", "u-02", "m-02",
             [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "bot.py\n"}],
             toolUseResult={"stdout": "bot.py\n", "stderr": ""}, sourceToolAssistantUUID="m-02"),
        user(S1, sub, "2026-10-30T09:05:00.000Z", "u-03", "u-02",
             "<local-command-caveat>Caveat: synthetic.</local-command-caveat>", isMeta=True),
        user(S1, sub, "2026-10-30T09:05:00.100Z", "u-04", "u-03",
             "<command-name>/model</command-name>\n<command-message>model</command-message>"),
        user(S1, sub, "2026-10-30T09:06:00.000Z", "u-05", "u-04",
             [{"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo="}},
              {"type": "text", "text": "The alert button overlaps the menu, fix the layout"}], **human),
        claude_base(S1, sub, "2026-10-30T09:06:30.000Z", "a-02", "u-05", type="attachment",
                    attachment={"type": "queued_command", "prompt": "also update the README",
                                "commandMode": "prompt", "origin": {"kind": "human"},
                                "source_uuid": "q-01", "timestamp": "2026-10-30T09:06:29.000Z"}),
        {"type": "last-prompt", "lastPrompt": "The alert button overlaps the menu, fix the layout",
         "leafUuid": "a-02", "sessionId": S1},
        assistant(S1, PROJECT, "2026-10-30T09:07:00.000Z", "m-03", "a-02", "msg_B", "req_2",
                  {"type": "text", "text": "Done: added /alert, fixed the layout and updated the README. "
                                           "Next: add tests for price thresholds."}, "end_turn", u2),
        claude_base(S1, PROJECT, "2026-10-30T09:07:00.500Z", "s-01", "m-03", type="system",
                    subtype="stop_hook_summary", level="suggestion", hookCount=0, hookInfos=[], hookErrors=[],
                    hookAdditionalContext=[], preventedContinuation=False, stopReason="", hasOutput=False,
                    toolUseID="t-01"),
        {"type": "custom-title", "customTitle": "Price alerts", "sessionId": S1},
        {"type": "ai-title", "aiTitle": "Add price alert command", "sessionId": S1},
        {"type": "agent-name", "agentName": "price-alerts", "sessionId": S1},
        {"type": "last-prompt", "lastPrompt": "The alert button overlaps the menu, fix the layout, and while you "
                                              "are there please also make sure the colors match the rest of the "
                                              "app, the fonts are consistent, and the spacing between the cards …",
         "leafUuid": "s-01", "sessionId": S1},
        {"type": "cost-state", "sessionId": S1, "totalCostUSD": 0.4321, "totalAPIDuration": 9000,
         "totalAPIDurationWithoutRetries": 9000, "totalToolDuration": 400, "totalLinesAdded": 42,
         "totalLinesRemoved": 7, "totalDuration": 420000, "startTime": 1793350800000, "hasUnknownModelCost": False,
         "modelUsage": {"claude-opus-5-5": {"inputTokens": 14, "outputTokens": 320, "cacheReadInputTokens": 10600,
                                            "cacheCreationInputTokens": 300, "webSearchRequests": 0,
                                            "costUSD": 0.4321, "thinkingTokens": 0}}},
    ]
    write_jsonl(pdir / f"{S1}.jsonl", s1)
    (pdir / S1).mkdir()
    (pdir / S1 / "custom-title.json").write_text(json.dumps({"customTitle": "Price alerts"}) + "\n")
    agent = "agent-a1b2c3d4"
    sub_recs = [
        dict(user(S1, PROJECT, "2026-10-30T09:01:00.000Z", "su-01", None, "Find where commands are registered"),
             isSidechain=True, agentId=agent),
        dict(assistant(S1, PROJECT, "2026-10-30T09:01:05.000Z", "sm-01", "su-01", "msg_S", "req_S",
                       {"type": "text", "text": "Commands are registered in bot.py."}, "end_turn",
                       usage(3, 40), model="claude-sonnet-5-5"),
             isSidechain=True, agentId=agent, attributionAgent="Explore"),
    ]
    write_jsonl(pdir / S1 / "subagents" / f"{agent}.jsonl", sub_recs)
    (pdir / S1 / "subagents" / f"{agent}.meta.json").write_text(json.dumps(
        {"agentType": "Explore", "description": "Find command registration", "spawnDepth": 1,
         "toolUseId": "toolu_9"}) + "\n")

    # Older, minimal session in the same project (CLI entrypoint, no last-prompt / cost-state).
    s2 = [
        dict(user(S2, PROJECT, "2026-10-20T18:00:00.000Z", "v-01", None, "Set up the Telegram bot skeleton",
                  version="2.1.209"), entrypoint="cli"),
        dict(assistant(S2, PROJECT, "2026-10-20T18:00:10.000Z", "w-01", "v-01", "msg_C", "req_3",
                       {"type": "text", "text": "Skeleton created."}, "end_turn", usage(5, 50)),
             entrypoint="cli", version="2.1.209"),
    ]
    write_jsonl(pdir / f"{S2}.jsonl", s2)

    # Defensive-parsing fixture: one malformed line and one unknown record type (synthetic; real logs had none).
    odir = CLAUDE / "projects" / "-home-user-dev-other"
    s3 = [
        user(S3, OTHER, "2026-09-01T12:00:00.000Z", "x-01", None, "Investigate the flaky test"),
        {"type": "some-future-record", "sessionId": S3, "whatever": [1, 2, 3]},
        assistant(S3, OTHER, "2026-09-01T12:00:30.000Z", "y-01", "x-01", "msg_D", "req_4",
                  {"type": "text", "text": "The test depends on wall-clock time."}, "end_turn", usage(5, 30)),
    ]
    write_jsonl(odir / f"{S3}.jsonl", s3, raw_lines={2: '{"type":"assistant","truncated'})
    (CLAUDE / "projects" / "-home-user-dev-My-Project" / "memory").mkdir()
    (CLAUDE / "projects" / "-home-user-dev-My-Project" / "memory" / "MEMORY.md").write_text("# synthetic\n")


# ---------------------------------------------------------------- Codex

T1 = "01a1aaaa-0000-7000-8000-000000000001"
W2 = "01a1aaaa-0000-7000-8000-0000000000w2".replace("w", "f")
G1 = "01a1aaaa-0000-7000-8000-0000000000g1".replace("g", "e")
T3 = "01a0bbbb-0000-7000-8000-000000000003"
META_ID = {"internal_chat_message_metadata_passthrough": {}}


def rec(ts, typ, payload, ordinal=None):
    r = {"timestamp": ts, "type": typ, "payload": payload}
    if ordinal is not None:
        r["ordinal"] = ordinal
    return r


def session_meta(ts, thread_id, cwd, version, thread_source="user", **extra):
    p = {"session_id": thread_id, "id": thread_id, "timestamp": ts, "cwd": cwd, "originator": "Codex Desktop",
         "cli_version": version, "model_provider": "openai",
         "base_instructions": {"text": "synthetic", "provenance": {"type": "default"}},
         "history_mode": "full", "context_window": {"window_id": thread_id}}
    if thread_source is not None:
        p["thread_source"] = thread_source
    p.update(extra)
    return p


def msg(role, text, kind="input_text", **extra):
    return dict({"type": "message", "role": role, "content": [{"type": kind, "text": text}]}, **META_ID, **extra)


def token_count(total_in, total_out, used_percent):
    tu = {"input_tokens": total_in, "cached_input_tokens": total_in // 2, "cache_write_input_tokens": 0,
          "output_tokens": total_out, "reasoning_output_tokens": total_out // 4,
          "total_tokens": total_in + total_out}
    return {"type": "token_count",
            "info": {"total_token_usage": tu, "last_token_usage": tu, "model_context_window": 258400},
            "rate_limits": {"limit_id": "codex", "limit_name": None,
                            "primary": {"used_percent": used_percent, "window_minutes": 10080,
                                        "resets_at": 1793900000},
                            "secondary": None,
                            "credits": {"has_credits": False, "unlimited": False, "balance": "0"},
                            "individual_limit": None, "spend_control_reached": None, "plan_type": "plus",
                            "rate_limit_reached_type": None}}


def build_codex() -> None:
    shutil.rmtree(CODEX, ignore_errors=True)
    git = {"commit_hash": "0123456789abcdef0123456789abcdef01234567", "branch": "main",
           "repository_url": "https://example.invalid/user/my-project.git"}
    turn_ctx = {"turn_id": "turn-1", "cwd": PROJECT, "workspace_roots": [PROJECT], "current_date": "2026-10-30",
                "timezone": "Europe/Rome", "approvals_reviewer": "user", "sandbox_policy": {"type": "workspace-write"},
                "permission_profile": {}, "file_system_sandbox_policy": {}, "model": "gpt-5.5", "comp_hash": "x",
                "personality": "default", "collaboration_mode": {}, "multi_agent_version": "2",
                "realtime_active": False, "summary": "auto", "approval_policy": "on-request", "effort": "medium"}
    t1 = [
        rec("2026-10-30T10:00:00.000Z", "session_meta",
            session_meta("2026-10-30T10:00:00.000Z", T1, PROJECT, "0.162.0-alpha.2", source="vscode", git=git), 0),
        rec("2026-10-30T10:00:00.010Z", "turn_context", turn_ctx, 1),
        rec("2026-10-30T10:00:00.020Z", "response_item", msg("developer", "<permissions instructions>synthetic"), 2),
        rec("2026-10-30T10:00:00.030Z", "response_item", msg("user", "# AGENTS.md instructions for the project\n..."), 3),
        rec("2026-10-30T10:00:00.040Z", "response_item", msg("user", "<environment_context>synthetic</environment_context>"), 4),
        rec("2026-10-30T10:00:00.050Z", "response_item", msg("user", "Wire the WireGuard tunnel to the agent loop"), 5),
        rec("2026-10-30T10:00:00.060Z", "event_msg", {"type": "task_started", "turn_id": "turn-1",
                                                      "started_at": 1793354400, "collaboration_mode_kind": "default",
                                                      "model_context_window": 258400}, 6),
        rec("2026-10-30T10:00:02.000Z", "response_item", dict({"type": "reasoning", "id": "rs_1", "summary": [],
                                                               "encrypted_content": "synthetic"}, **META_ID), 7),
        rec("2026-10-30T10:00:03.000Z", "response_item", dict({"type": "custom_tool_call", "id": "ctc_1",
                                                               "status": "completed", "call_id": "call_1",
                                                               "name": "exec", "input": "ls"}, **META_ID), 8),
        rec("2026-10-30T10:00:04.000Z", "response_item", dict({"type": "custom_tool_call_output", "id": "ctco_1",
                                                               "call_id": "call_1",
                                                               "output": [{"type": "input_text", "text": "tunnel.rs"}]},
                                                              **META_ID), 9),
        rec("2026-10-30T10:00:05.000Z", "event_msg", token_count(12000, 800, 31.0), 10),
        rec("2026-10-30T10:00:06.000Z", "response_item", msg("assistant", "Checking the tunnel module.",
                                                             kind="output_text", phase="commentary"), 11),
        rec("2026-10-30T10:00:20.000Z", "response_item", msg("assistant", "Tunnel wired; handshake retry still TODO.",
                                                             kind="output_text", phase="final_answer"), 12),
        rec("2026-10-30T10:00:20.010Z", "event_msg", {"type": "agent_message", "message":
                                                      "Tunnel wired; handshake retry still TODO.",
                                                      "phase": None, "memory_citation": None}, 13),
        rec("2026-10-30T10:00:20.020Z", "event_msg", {"type": "task_complete", "turn_id": "turn-1",
                                                      "started_at": 1793354400, "completed_at": 1793354420,
                                                      "duration_ms": 20000, "time_to_first_token_ms": 900,
                                                      "last_agent_message": "Tunnel wired; handshake retry still TODO."}, 14),
        rec("2026-10-30T10:00:20.030Z", "token_usage_record",
            {"thread_id": T1, "turn_id": "turn-1", "session_id": T1, "root_turn_id": "turn-1",
             "response_id": "resp_1", "usage": {}, "turn_token_usage": {}, "thread_token_usage": {}}),
    ]
    day = CODEX / "sessions" / "2026" / "10" / "30"
    write_jsonl(day / f"rollout-2026-10-30T10-00-00-{T1}.jsonl", t1)

    # Continuation of the same thread in a second file (name: <thread>_<uuid>), with history_base.
    cont = [
        rec("2026-10-30T15:00:00.000Z", "session_meta",
            session_meta("2026-10-30T15:00:00.000Z", T1, PROJECT, "0.162.0-alpha.2", source="vscode", git=git,
                         history_base={"thread_id": T1, "end_ordinal_exclusive": 15, "end_byte_offset": 9999}), 15),
        rec("2026-10-30T15:00:00.010Z", "turn_context", dict(turn_ctx, turn_id="turn-2"), 16),
        rec("2026-10-30T15:00:00.020Z", "response_item", msg("user", "Now add the handshake retry with backoff"), 17),
        rec("2026-10-30T15:00:30.000Z", "event_msg", token_count(30000, 2100, 34.5), 18),
        rec("2026-10-30T15:00:31.000Z", "event_msg", {"type": "task_complete", "turn_id": "turn-2",
                                                      "started_at": 1793372400, "completed_at": 1793372431,
                                                      "duration_ms": 31000, "time_to_first_token_ms": 700,
                                                      "last_agent_message": "Retry with exponential backoff added."}, 19),
    ]
    write_jsonl(day / f"rollout-2026-10-30T15-00-00-{T1}_{W2}.jsonl", cont)

    # Automatic guardian review subagent: must not count as user activity.
    guardian = [
        rec("2026-10-31T08:00:00.000Z", "session_meta",
            session_meta("2026-10-31T08:00:00.000Z", G1, PROJECT, "0.162.0-alpha.2", thread_source="guardian_review",
                         parent_thread_id=T1, source={"subagent": {"other": "guardian"}}), 0),
        rec("2026-10-31T08:00:00.010Z", "response_item", msg("user", "Review the pending command for safety"), 1),
        rec("2026-10-31T08:00:05.000Z", "event_msg", {"type": "task_complete", "turn_id": "g-1",
                                                      "started_at": 1793433600, "last_agent_message": "approve"}, 2),
    ]
    sub_g = dict(guardian[0]["payload"]); sub_g["session_id"] = T1  # subagents: id != session_id
    guardian[0]["payload"] = sub_g
    write_jsonl(CODEX / "sessions" / "2026" / "10" / "31" / f"rollout-2026-10-31T08-00-00-{G1}.jsonl", guardian)

    # Archived older-version session: no thread_source, prompt also mirrored as event_msg/user_message.
    old = [
        {"timestamp": "2026-09-13T16:00:12.000Z", "type": "session_meta",
         "payload": session_meta("2026-09-13T16:00:12.000Z", T3, OTHER, "0.153.4", thread_source=None)},
        {"timestamp": "2026-09-13T16:00:13.000Z", "type": "response_item",
         "payload": msg("user", "Why does the scraper time out?")},
        {"timestamp": "2026-09-13T16:00:13.010Z", "type": "event_msg",
         "payload": {"type": "user_message", "message": "Why does the scraper time out?", "local_images": [],
                     "local_audio": [], "text_elements": []}},
        {"timestamp": "2026-09-13T16:01:00.000Z", "type": "event_msg",
         "payload": {"type": "task_complete", "turn_id": "t", "started_at": 1789315212,
                     "last_agent_message": None}},
    ]
    write_jsonl(CODEX / "archived_sessions" / f"rollout-2026-09-13T16-00-12-{T3}.jsonl", old)

    (CODEX / "session_index.jsonl").write_text(
        json.dumps({"id": T1, "thread_name": "WireGuard agent loop", "updated_at": "2026-10-30T15:00:31.000Z"},
                   separators=(",", ":")) + "\n")


if __name__ == "__main__":
    build_claude()
    build_codex()
    print("fixtures regenerated")
