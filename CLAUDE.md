# Purser — Agent Swarm Coordination

## What is this project?
Nostr-native payment daemon replacing Zaprite. Rust only. See `spec.md` for full requirements.

## Issue tracker
All work is tracked in GitHub issues on `EthnTuttle/purser`. Issues are labeled by phase:
- `phase:0-scaffold` — sequential foundation (must complete first)
- `phase:1-parallel` — fully parallel agent work (7 issues)
- `phase:2-integration` — wiring modules together (2 issues)
- `phase:3-ship` — tests, deployment, docs (4 issues)

## Module ownership (agent swarm boundaries)

Each Phase 1 issue owns **exclusive files**. Agents must not edit files outside their ownership boundary. This prevents merge conflicts across parallel agents.

| Issue | Module | Files owned |
|---|---|---|
| #2 Config & catalog | config, catalog | `src/config.rs`, `src/catalog.rs` |
| #3 Message validation | messages | `src/messages/validation.rs` |
| #4 MDK communication | nostr | `src/nostr/**` |
| #5 SquareProvider | square | `src/providers/square.rs` |
| #6 StrikeProvider | strike | `src/providers/strike.rs` |
| #7 Polling engine | polling | `src/polling.rs` |
| #8 Rate limiter | ratelimit | `src/ratelimit.rs` |

**Shared files (Phase 0 only, read-only after):**
- `src/providers/mod.rs` — trait + shared types
- `src/messages/mod.rs` — message structs
- `src/state.rs` — `AppState`
- `src/error.rs` — error types

If you need to change a shared type, open an issue — do not modify shared files in a Phase 1 branch.

## Key dependencies
- `mdk-core` (marmot-protocol/mdk) — pinned to specific commit hash (TBD before impl)
- `tokio` — async runtime
- `reqwest` — HTTP client for payment provider APIs
- `serde` / `serde_json` — serialization
- `chrono` — timestamps

## Conventions
- All prices are `String` (decimal-as-string), never `f64`
- Monotonic timers (`tokio::time::Instant`) for polling/backoff; wall clock (`chrono::Utc::now()`) for message timestamps only
- All encrypted messaging via MDK — no raw NIP-44 or NIP-04
- No public HTTP endpoints. Polling only for payment status checks.
- Config from `config.toml` + `.env`. Secrets only in `.env`, never in code.

## For agents: how to work on an issue
1. Read this file and `spec.md`
2. Check your issue's "Depends on" — ensure those are merged
3. Branch from `master` as `issue-N-short-name`
4. Only edit files listed in your issue's "Files owned" section
5. Write tests alongside your implementation
6. PR back to `master` referencing the issue number
