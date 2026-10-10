# AGENTS.md — Pixel Quest (backend)

Guidance for AI coding agents working in this repo. `CLAUDE.md` imports this file.

## What this is

The Rust API for Pixel Quest, a D&D 5e campaign app. It owns accounts, characters, campaigns and
**the live table**: per-campaign maps, initiative, server-side dice, the event feed, sessions, and
the projection that decides what each player is allowed to see. The React frontend lives in the
sibling repo `michelsen93/dnd` (checked out at `../dnd`); its `AGENTS.md` describes the product and
its **`docs/GAMEPLAY.md` holds the gameplay design, the endpoint table, and the roadmap**.

Stack: Rust 2024 edition, Axum 0.8, SQLx 0.8 (SQLite, runtime queries — no compile-time
`query!` macros), Firebase ID tokens verified with `jsonwebtoken` (Google keys fetched via
`reqwest`), server-side sessions in an encrypted private cookie (`axum-extra`), SSE over a
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

Config also reads `PORT` (Cloud Run) when `APP_PORT` is unset, `COOKIE_SECURE=true` (marks the
session cookie `Secure`) and `FIREBASE_PROJECT_ID`. The server refuses to start with
`COOKIE_SECURE=true` and no `COOKIE_SECRET` or no `FIREBASE_PROJECT_ID`.

## Auth model

- **Production:** the browser signs in with Firebase Auth and posts the ID token to
  `POST /api/auth/session`. `firebase.rs` verifies it (RS256, Google's JWKS cached, `aud` = project,
  `iss`, `exp`, and `email_verified`), then the user is found by `firebase_uid`, else linked by
  email, else created.
- **Local dev / tests / e2e:** with no `FIREBASE_PROJECT_ID`, `POST /api/auth/dev-login {email}`
  signs in without a password. It returns 404 when Firebase is configured. `GET /api/auth/config`
  tells the frontend which mode is active.
- **Sessions** (`auth.rs`): a random 32-byte token in the `__session` cookie; the `sessions` table
  stores its SHA-256 with a 30-day sliding idle expiry (touched at most hourly) and a 90-day cap.
  `require_user(&state.pool, &jar)` is the only way handlers learn who's calling.
  `/logout`, `/logout-all`, `/export` (JSON of everything about the user) and `DELETE /account`
  (cascade + Firebase user deletion via the metadata-server token) complete the set.

## Abuse limits (`limits.rs`)

The router built by `routes::app(state)` adds a 256 KB body limit and the `rate_limit` middleware
(in-memory fixed windows: 300 req/min per session, 60/min per IP when signed out, 10/min for
sign-in, TV pairing and invite-code joins; a TV screen's token counts like a session). Handlers enforce quotas with `ensure_quota` (characters,
campaigns owned, members, maps, entities, notes; the feed is pruned to 5,000 events) and sizes with
`ensure_size` / `ensure_chars`. New create endpoints need a quota, new free-text fields a cap.
Database errors are logged and returned as a generic 500.

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
  main.rs            binary: tracing, config, startup guards, pool, migrations, CORS, no-store header,
                     serve (with client addresses), SIGTERM exit
  lib.rs             module tree (tests build the app from here)
  config.rs          AppConfig::from_env
  state/mod.rs       AppState { pool, cookie_key, config, campaign_tx, firebase, limiter } + new / notify()
  auth.rs            server-side sessions (__session cookie, hashed tokens), require_user()
  firebase.rs        FirebaseVerifier: ID token verification, Firebase user deletion
  limits.rs          quotas, size caps, RateLimiter + rate_limit middleware
  access.rs          campaign_access() → CampaignAccess { campaign, role: Dm | Player }, require_dm()
  dice.rs            dice notation parser/roller (mirrors ../dnd/src/utils/dice.ts), unit-tested
  projection.rs      player visibility (LOS) and the player projection of an encounter, unit-tested
  repo.rs            data helpers: table state, encounters, characters (as JSON), events
  db/mod.rs          connect(), payload (de)serialize, merge_json (shallow PATCH merge)
  error.rs           ApiError → JSON { "error": "..." } (bad_request/unauthorized/forbidden/not_found/conflict)
  models/mod.rs      wire types (serde camelCase): Character, Encounter, TableState, Combat, …
  routes/
    mod.rs           app(state): router + body limit + rate limit; campaign routers merged under /api/campaigns
    auth.rs          /api/auth/{config,session,dev-login,logout,logout-all,me,export,account}
    characters.rs    /api/characters[/{id}] (owner-scoped; changes notify the character's campaigns)
    notes.rs         /api/characters/{id}/notes, /api/notes/{id}
    campaigns.rs     list (role, live), create/rename/delete, members, invite, join, party sheets
    encounters.rs    /{id}/encounters — DM map CRUD with optimistic concurrency (revision → 409)
    entities.rs      /{id}/entities — quests/NPCs/places/handouts/loot, reveal, claim loot
    sessions.rs      /{id}/sessions — start, end (publish log), edit log
    table.rs         /{id}/table (snapshot), /stream (SSE), /events, /rolls (targets → hit/miss), /actions
    screen.rs        TV screens: DM /{id}/screens (pairing codes); /api/screen/{pair, {token}/table, {token}/stream}
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
  filtered. Paired TV screens get the same party view (`screen.rs`) with public events only. Any
  new DM-only data needs the same treatment **and a test**.
- **Shared state changes go through commands**, not blob PATCHes: add an `Action` variant in
  `routes/table.rs`, record an event with `Ctx::event(kind, visibility, payload)`, and the handler
  calls `state.notify(campaign_id, kind)` so SSE clients refetch.
- **Event visibility:** `public` (everyone), `private` (actor + DM), `dm` (DM only).
- **Errors:** `ApiError::{bad_request, unauthorized, forbidden, not_found, conflict}`; `?` converts
  sqlx/serde errors (sqlx errors are logged and become a generic 500).
- **Migrations:** add a new numbered file; never edit an applied one. TEXT uuid ids, ISO-8601 TEXT
  timestamps (`models::now_iso()`), `ON DELETE CASCADE`. Foreign keys are enabled per connection
  in `db::connect`. SQLite JSON1 (`json_extract`, `json_each`) is available.
- **Routes:** Axum 0.8 path syntax `/{id}`; campaign routes live in their module's `router()` and
  are merged in `routes/mod.rs`. Nested routers have **no trailing slash** (`/api/campaigns`).

## Testing

Use `tests/common/mod.rs`: `test_app()` gives an in-memory app (dev sign-in mode),
`test_app_with(|state| …)` customizes state (e.g. a `FirebaseVerifier::with_static_keys` for token
tests; `auth_integration.rs` generates an RSA key at runtime, never commit one),
`app.register(email)` signs in via dev-login and returns a cookie, `app.call(method, uri, cookie, body)` returns `(StatusCode, Value)`, `app.ok(...)` asserts
success, plus `character_json` and `corridor_encounter` fixtures. `tests/table_integration.rs`
shows the pattern (a `Table` fixture with a DM, a player and a joined character).

Every new endpoint needs: happy path, unauthenticated (401), outsider (404), and player-vs-DM
(403 or filtered output). For anything players see, assert what they **don't** receive.

In-memory pools are single-connection (`db::connect` detects `:memory:`), because each SQLite
memory connection is its own database.

## Known issues / tech debt

- `COOKIE_SECRET` is padded/truncated to 64 bytes; the default secret is for dev only.
- Rate limits are in memory, so they reset on restart (fine for a single instance). The client IP
  comes from `X-Forwarded-For`, which only guards anonymous endpoints.
- TV screen tokens travel in the URL path (EventSource can't send headers), so they appear in
  request logs. They are read-only, revocable by the DM and expire after 30 idle days.
- SSE uses one broadcast channel for all campaigns, filtered per subscriber — fine at table scale.
- `list_campaigns` and the snapshot run several small queries; fine for a party of 6.

## Working agreement for agents

1. API changes go together with the matching change in `../dnd` (types in `src/types`, store in
   `src/store`) and an update to the endpoint table in `../dnd/docs/GAMEPLAY.md` §5.
2. `cargo fmt && cargo clippy --all-targets && cargo test` must be clean before committing.
3. One roadmap item per PR; tick it in `../dnd/docs/GAMEPLAY.md`.
