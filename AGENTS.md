# AGENTS.md — Pixel Quest (backend)

Guidance for AI coding agents working in this repo. `CLAUDE.md` imports this file.

## What this is

The Rust API for Pixel Quest, a D&D 5e campaign app. It owns accounts, characters, campaigns and
**the live table**: per-campaign maps, initiative, server-side dice, the event feed, sessions, and
the projection that decides what each player is allowed to see. The React frontend lives in the
sibling repo `michelsen93/dnd` (checked out at `../dnd`); its `AGENTS.md` describes the product and
its **`docs/GAMEPLAY.md` holds the gameplay design, the endpoint table, and the roadmap**.

Stack: Rust 2024 edition, Axum 0.8, SQLx 0.8 (SQLite, runtime queries — no compile-time
`query!` macros), argon2, encrypted private cookies (`axum-extra`), SSE over a
`tokio::sync::broadcast` channel, `rand` for dice.

## Commands

```bash
cargo build
cargo test                                   # unit tests + integration tests in tests/
cargo test --test table_integration combat   # one test (substring match)
cargo clippy --all-targets                   # must stay at zero warnings
cargo fmt                                    # format before committing
cargo run                                    # 127.0.0.1:3001, runs migrations on boot
DATABASE_URL="sqlite:///tmp/pq-e2e.sqlite?mode=rwc" cargo run   # throwaway DB for e2e
```

Config comes from env / `.env` (see `.env.example`): `APP_HOST`, `APP_PORT`, `DATABASE_URL`
(default `sqlite://./dnd.sqlite?mode=rwc`, git-ignored), `ALLOWED_ORIGIN`, `COOKIE_SECRET`.
Cloud sessions pre-build via `.claude/hooks/session-start.sh`.

Config also reads `PORT` (Cloud Run) when `APP_PORT` is unset, and `COOKIE_SECURE=true` marks the
session cookie `Secure` (the server refuses to start with `COOKIE_SECURE=true` and no
`COOKIE_SECRET`).

## Deployment (Cloud Run behind Firebase Hosting)

Production runs this crate as a container on Cloud Run, reached through Firebase Hosting rewrites
on the same domain. The deploy script and guide live in the frontend repo
(`../dnd/deploy/deploy.sh`, `../dnd/docs/DEPLOY.md`).

- `Dockerfile`: multi-stage build (`rust:1.95-bookworm` → `debian:bookworm-slim`), no apt; ships
  the binary, Litestream and CA roots. `docker-entrypoint.sh` restores the SQLite file from
  `LITESTREAM_REPLICA_URL` (e.g. `gcs://bucket/dnd.sqlite`) on boot, then runs the server under
  `litestream replicate -exec`, which streams every change back to Cloud Storage.
- **Exactly one instance** (`--max-instances 1`): SQLite and the in-process broadcast channel for
  SSE don't work across instances. Don't add features that assume horizontal scaling.
- The server exits immediately on SIGTERM (no draining: SSE streams never end) so Litestream can
  run its final sync within Cloud Run's 10 s shutdown window.
- The session cookie is named `__session` because Firebase Hosting forwards only that cookie.
- Every response gets `Cache-Control: no-store` (unless a handler sets one) so the Hosting CDN
  never caches per-user data.
- SSE sends a named `ping` event on connect and every 10 s (with `retry: 1500`). Clients use the
  pings to detect a buffering proxy and fall back to polling; keep them.
- Test the image locally: `docker build -t pixel-quest-api .` then run it with
  `LITESTREAM_REPLICA_URL=file:///replica/dnd.sqlite` and a mounted folder; stop/start and the
  data must come back.

For end-to-end checks run the frontend playtest (`npm run playtest` in `../dnd`, see its
AGENTS.md) against a backend on a throwaway database.

## Layout

```
src/
  main.rs            binary: tracing, config, pool, migrations, CORS, no-store header, serve, SIGTERM exit
  lib.rs             module tree (tests build the router from here)
  config.rs          AppConfig::from_env
  state/mod.rs       AppState { pool, cookie_key, config, campaign_tx } + AppState::new / notify()
  auth.rs            password hashing, session cookie, require_user()
  access.rs          campaign_access() → CampaignAccess { campaign, role: Dm | Player }, require_dm()
  dice.rs            dice notation parser/roller (mirrors ../dnd/src/utils/dice.ts), unit-tested
  projection.rs      player visibility (LOS) and the player projection of an encounter, unit-tested
  repo.rs            data helpers: table state, encounters, characters (as JSON), events
  db/mod.rs          connect(), payload (de)serialize, merge_json (shallow PATCH merge)
  error.rs           ApiError → JSON { "error": "..." } (bad_request/unauthorized/forbidden/not_found/conflict)
  models/mod.rs      wire types (serde camelCase): Character, Encounter, TableState, Combat, …
  routes/
    mod.rs           router(): campaign routers are merged under /api/campaigns
    auth.rs          /api/auth/{register,login,logout,me}
    characters.rs    /api/characters[/{id}] (owner-scoped; changes notify the character's campaigns)
    notes.rs         /api/characters/{id}/notes, /api/notes/{id}
    campaigns.rs     list (role, live), create/rename/delete, members, invite, join, party sheets
    encounters.rs    /{id}/encounters — DM map CRUD with optimistic concurrency (revision → 409)
    entities.rs      /{id}/entities — quests/NPCs/places/handouts/loot, reveal, claim loot
    sessions.rs      /{id}/sessions — start, end (publish log), edit log
    table.rs         /{id}/table (snapshot), /stream (SSE), /events, /rolls, /actions
migrations/          000N_name.sql, applied by sqlx::migrate! at startup and in tests
Dockerfile           production image (server + Litestream); docker-entrypoint.sh restores/replicates
tests/               common/ helpers + end-to-end tests through the real router
```

## Conventions

- **Storage model:** domain objects are JSON `payload TEXT` plus indexed columns. Rust structs in
  `models/` are the schema; every new field gets `#[serde(default)]`. `Character`, `Encounter`,
  monsters and tokens carry `#[serde(flatten)] extra` so fields owned by the frontend survive a
  round trip — don't remove that.
- **Auth first:** every handler takes `PrivateCookieJar` and calls `require_user`. Campaign-scoped
  handlers then call `campaign_access(&state.pool, &id, &user.id)` (404 for outsiders) and
  `.require_dm()` (403) where needed. Resources the caller can't see are 404, not 403.
- **Hidden information is enforced here.** Players get `project_encounter_for_player` (no unseen
  monsters, no HP/AC, `fog` terrain), unseen monsters are removed from combat, events about them
  are `dm` visibility, hidden DCs are stripped, `dmNotes` are stripped, unrevealed entities are
  filtered. Any new DM-only data needs the same treatment **and a test**.
- **Shared state changes go through commands**, not blob PATCHes: add an `Action` variant in
  `routes/table.rs`, record an event with `Ctx::event(kind, visibility, payload)`, and the handler
  calls `state.notify(campaign_id, kind)` so SSE clients refetch.
- **Event visibility:** `public` (everyone), `private` (actor + DM), `dm` (DM only).
- **Errors:** `ApiError::{bad_request, unauthorized, forbidden, not_found, conflict}`; `?` converts
  sqlx/serde/argon2 errors.
- **Migrations:** add a new numbered file; never edit an applied one. TEXT uuid ids, ISO-8601 TEXT
  timestamps (`models::now_iso()`), `ON DELETE CASCADE`. Foreign keys are enabled per connection
  in `db::connect`. SQLite JSON1 (`json_extract`, `json_each`) is available.
- **Routes:** Axum 0.8 path syntax `/{id}`; campaign routes live in their module's `router()` and
  are merged in `routes/mod.rs`. Nested routers have **no trailing slash** (`/api/campaigns`).

## Testing

Use `tests/common/mod.rs`: `test_app()` gives an in-memory app, `app.register(email)` returns a
cookie, `app.call(method, uri, cookie, body)` returns `(StatusCode, Value)`, `app.ok(...)` asserts
success, plus `character_json` and `corridor_encounter` fixtures. `tests/table_integration.rs`
shows the pattern (a `Table` fixture with a DM, a player and a joined character).

Every new endpoint needs: happy path, unauthenticated (401), outsider (404), and player-vs-DM
(403 or filtered output). For anything players see, assert what they **don't** receive.

In-memory pools are single-connection (`db::connect` detects `:memory:`), because each SQLite
memory connection is its own database.

## Known issues / tech debt

- The session cookie value is the user id (encrypted + http-only) with no expiry or server-side
  revocation. `COOKIE_SECRET` is padded/truncated to 64 bytes; the default secret is for dev only.
- SSE uses one broadcast channel for all campaigns, filtered per subscriber — fine at table scale.
- `list_campaigns` and the snapshot run several small queries; fine for a party of 6.

## Working agreement for agents

1. API changes go together with the matching change in `../dnd` (types in `src/types`, store in
   `src/store`) and an update to the endpoint table in `../dnd/docs/GAMEPLAY.md` §5.
2. `cargo fmt && cargo clippy --all-targets && cargo test` must be clean before committing.
3. One roadmap item per PR; tick it in `../dnd/docs/GAMEPLAY.md`.
