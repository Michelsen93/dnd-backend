# AGENTS.md — Pixel Quest (backend)

Guidance for AI coding agents working in this repo. `CLAUDE.md` imports this file.

## What this is

The Rust API for Pixel Quest, a D&D 5e campaign companion. The React frontend lives in the
sibling repo `michelsen93/dnd` (checked out at `../dnd`), whose `AGENTS.md` describes the product
and whose **`docs/GAMEPLAY.md` holds the gameplay design and roadmap** — read it before feature
work.

Stack: Rust 2024 edition, Axum 0.8, SQLx 0.8 (SQLite, runtime queries — no compile-time
`query!` macros), argon2 password hashing, encrypted private cookies (`axum-extra`), SSE via a
`tokio::sync::broadcast` channel.

## Commands

```bash
cargo build
cargo test                       # integration tests in tests/, in-memory SQLite
cargo test --test campaigns_integration <name>   # one test
cargo clippy --all-targets       # keep warnings at zero in files you touch
cargo fmt                        # format before committing
cargo run                        # listens on 127.0.0.1:3001, runs migrations on boot
```

Config comes from env / `.env` (see `.env.example`): `APP_HOST`, `APP_PORT`, `DATABASE_URL`
(default `sqlite://./dnd.sqlite?mode=rwc`), `ALLOWED_ORIGIN` (default `http://localhost:5173`),
`COOKIE_SECRET`. Cloud sessions pre-build via `.claude/hooks/session-start.sh`.

## Layout

```
src/
  main.rs          binary: tracing, config, pool, migrations, CORS, serve
  lib.rs           re-exports modules so tests can build the router
  config.rs        AppConfig::from_env
  state/mod.rs     AppState { pool, cookie_key, config, encounter_sync_tx }
  auth.rs          hash/verify password, session cookie, require_user()
  db/mod.rs        connect(), payload (de)serialize, merge_json (shallow PATCH merge)
  error.rs         ApiError → JSON { "error": "..." } with status
  models/mod.rs    wire types (serde camelCase) — mirror of ../dnd/src/types
  routes/
    mod.rs         router(): all route registration lives here
    auth.rs        /api/auth/{register,login,logout,me}
    characters.rs  /api/characters[/{id}]            (owner-scoped)
    notes.rs       /api/characters/{id}/notes, /api/notes/{id}
    combat.rs      /api/combat/sessions[...], /api/combat/entries/{id}  (unused by frontend)
    encounters.rs  /api/encounters/state (GET/PATCH whole blob), /api/encounters/stream (SSE)
    campaigns.rs   /api/campaigns, members, invite regenerate, join by code
migrations/        000N_name.sql, applied by sqlx::migrate! at startup and in tests
tests/             end-to-end tests through the real router
```

## Conventions

- **Storage model:** most domain objects are stored as a JSON `payload TEXT` column plus a few
  indexed columns (`id`, `user_id`, timestamps). Rust structs in `models/` are the schema; use
  `#[serde(default)]` for every new field so old payloads keep deserializing (see
  `characters_inventory_spells_integration::legacy_equipment_payload...`).
- **PATCH = shallow merge** via `db::merge_json`: top-level keys in the patch replace keys in the
  stored payload. Arrays are replaced wholesale.
- **Auth:** every handler takes `PrivateCookieJar` and calls `require_user(&state.pool, &jar)`
  first. Ownership checks are explicit SQL `WHERE ... AND user_id = ?`; return `404` (not 403)
  for resources the caller can't see.
- **Campaign authorization (when you add campaign-scoped data):** write a single helper that
  returns the caller's role in a campaign — `Dm` if `campaigns.owner_user_id = user`, `Player` if
  they have a `campaign_members` row, else `404`. DM-only data (monster HP, hidden fog, DM notes)
  must be **filtered server-side** for players, not just hidden in the UI.
- **Errors:** return `ApiError::{bad_request, unauthorized, not_found}` or `ApiError::new(status,
  msg)`. `?` converts sqlx/serde/argon2 errors.
- **Migrations:** add a new numbered file; never edit an applied one. SQLite: `TEXT` ids (uuid
  v4), ISO-8601 `TEXT` timestamps from `models::now_iso()`, `ON DELETE CASCADE` foreign keys
  (`PRAGMA foreign_keys = ON` is set in `db::connect`).
- **Routes:** register in `routes/mod.rs` or a sub-`router()`; use Axum 0.8 path syntax
  `/{id}`. Methods allowed by CORS: GET, POST, PATCH, DELETE.
- **Live updates:** `state.encounter_sync_tx` broadcasts a `String` key; SSE handlers filter by
  key. Today the key is a `user_id`. When sharing with a table, broadcast a `campaign_id` and
  authorize the subscriber as a member.

## Testing

Tests build the real router with `routes::router().with_state(state)` against
`sqlite::memory:` and drive it with `tower::ServiceExt::oneshot`. Copy the helpers at the top of
`tests/campaigns_integration.rs` (`test_app`, `register_and_get_cookie`, `response_json`,
`first_cookie`). Every new endpoint needs: happy path, unauthenticated (401), and
cross-user access (404). For campaign features, also test DM vs player visibility.

Gotcha: `db::connect` opens a pool of up to 5 connections and **each `sqlite::memory:` connection
is a separate database**. Tests pass today because traffic is sequential, but if you see
"no such table" in tests, use `sqlite:file:memdb_<uuid>?mode=memory&cache=shared` or a
`max_connections(1)` pool for tests.

## Known issues / tech debt

- The session cookie value is the raw user id (encrypted + http-only, but no expiry or server-side
  revocation). `COOKIE_SECRET` is padded/truncated to 64 bytes in `main.rs`; the default secret
  is insecure — fine for local, not for deployment.
- `encounter_states` is one JSON blob per **user**, not per campaign, so players can't see the
  DM's battlefield. `combat_*` tables are unused by the frontend.
- Quests, NPCs and session logs have no backend at all yet.
- `dnd.sqlite` is committed; treat it as a dev fixture, don't rely on its contents, and don't
  commit changes to it (`*.sqlite-wal`/`-shm` too).
- `list_campaigns` does an N+1 member query — fine at table scale.

## Working agreement for agents

1. API changes go together with the matching change in `../dnd` (types in `src/types`, store in
   `src/store`). Keep field names camelCase on the wire.
2. `cargo fmt && cargo clippy --all-targets && cargo test` must be clean before committing.
3. One roadmap item per PR; tick it in `../dnd/docs/GAMEPLAY.md`.
