# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is SALSA

A web application for controlling radio telescopes at Onsala Space Observatory. Users book time slots, point telescopes at targets, record spectra, and download observation data. Built in Rust with Axum, Askama templates, HTMX, and Tailwind CSS.

## Build and Development

```bash
cargo build          # Build (also downloads Tailwind CLI and compiles CSS)
cargo run -- -c config/ -d data/   # Run server (needs config.toml + .secrets.toml)
cargo test           # Run all tests
cargo test coords    # Run tests matching "coords"
```

- **Templates** (`templates/`) are compiled into the binary by Askama — changes require `cargo build` + server restart
- **Static assets** (`assets/`) are read at runtime — CSS, JS, HTML fragments do NOT require a rebuild
- **CSS** is built from `assets/style.src.css` via the Tailwind v4 standalone binary, automatically downloaded and run by `build.rs`
- Prefer standalone binaries over npm/Node.js for frontend tooling

## Architecture

**AppState** holds shared state: SQLite connection (`Arc<Mutex<Connection>>`), telescope collection, secrets, config, TLE cache, weather cache. Passed to route handlers via Axum's `State` extractor.

**Routes** (`src/routes/`) are modular — each file handles one feature area, registered via `.nest()` in `app.rs`. Content pages (about, support, experiments, technical) read HTML fragments from `assets/` and wrap them in the Askama layout.

**Telescope abstraction** — `Telescope` trait with two implementations: `SalsaTelescope` (real hardware via UHD/ROT2PROG) and `FakeTelescope` (simulated, for development). `TelescopeCollectionHandle` wraps `Arc<RwLock<HashMap>>`.

**Background tasks** spawned at startup: TLE satellite data refresh, weather cache refresh, booking monitor.

**Session middleware** extracts cookies, validates sessions, and populates `Extension<Option<User>>` into requests. Admin status is checked against `admin_config.user_ids` at request time.

## Styling Guidelines

- **Colors** are defined via Tailwind v4 `@theme` in `style.src.css` as semantic tokens (e.g. `accent`, `danger`, `success`, `warning`, `info`, `callout`). Use these instead of raw Tailwind palette colors like `indigo-600` or `red-600`. See `assets/colors.html` for the full palette.
- **Buttons** use the `.btn` component class (in `@layer components`). Default is accent-colored. Override with utilities: `class="btn bg-danger hover:bg-danger-hover"`. Don't build buttons from raw utilities.
- **Links** inside `.section` are automatically accent-colored with underline on hover via CSS. Don't add `class="underline"` or `class="text-accent hover:underline"` — the unlayered CSS overrides them anyway.

## Database

SQLite with Refinery migrations in `src/database.rs`. Tables: user, local_user, session, booking, observation, pending_oauth2, telescope_maintenance.
