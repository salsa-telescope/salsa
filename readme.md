# SALSA

Web-based control system for the [SALSA radio telescopes](https://salsa.oso.chalmers.se)
at Onsala Space Observatory. Users book time slots, point telescopes at targets,
record spectra, and download observation data.

Built with Rust, [Axum](https://github.com/tokio-rs/axum), [Askama](https://github.com/djc/askama)
templates, [HTMX](https://htmx.org), and [Tailwind CSS](https://tailwindcss.com).

## Building

```bash
cargo build
```

Tailwind CSS is compiled automatically via `build.rs` (the standalone Tailwind binary
is downloaded on first build).

## Configuration

The server requires two config files in the config directory, both gitignored:

- `config.toml` — telescope definitions, booking limits, admin user IDs
- `.secrets.toml` — OAuth2 provider credentials, webcam credentials

Example files are included in the repository as a starting point:

```bash
cp config.toml.example config/config.toml
cp .secrets.toml.example config/.secrets.toml
```

## Running

```bash
cargo run -- -c config/ -d data/
```

- `-c` — directory containing `config.toml` and `.secrets.toml`
- `-d` — directory where the SQLite database will be stored

## Testing

```bash
cargo test
```

## Development notes

**Templates** (`templates/`) are compiled into the binary by Askama — changes
require `cargo build` and a server restart. **Static assets** (`assets/`) are read
at runtime and do not require a rebuild.

**Fake telescopes** (simulated hardware, no UHD required) are supported for
development. Set `telescope_type = "Fake"` in `config.toml` — the example config
includes two fake telescopes by default.

**Safari + localhost**: Safari rejects secure cookies over plain HTTP. Use Chrome
or Firefox for local development.

## Deployment

The server runs as a systemd service on `salsa.oso.chalmers.se`.

View logs:
```bash
sudo journalctl -u salsa -f -p warning
```

Restart after config changes:
```bash
sudo systemctl restart salsa
```

## Architecture

- **Routes** (`src/routes/`) — one file per feature area, registered in `app.rs`
- **Models** (`src/models/`) — database models and telescope abstraction (`SalsaTelescope` / `FakeTelescope`)
- **AppState** — shared state: database connection, telescope handles, config, TLE and weather caches
- **Background tasks** — TLE satellite data refresh, weather cache refresh, booking monitor
- **Database** — SQLite with [Refinery](https://github.com/rust-db/refinery) migrations in `src/database.rs`
