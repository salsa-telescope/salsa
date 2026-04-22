# SALSA

Web-based control system for the [SALSA radio telescopes](https://salsa.oso.chalmers.se)
at Onsala Space Observatory. Users book time slots, point telescopes at targets,
record spectra, and download observation data.

Built with Rust, [Axum](https://github.com/tokio-rs/axum), [Askama](https://github.com/djc/askama)
templates, [HTMX](https://htmx.org), and [Tailwind CSS](https://tailwindcss.com).

## Building

On Debian/Ubuntu, install the build dependencies first:

```bash
sudo apt install libuhd-dev libuhd4.6.0 clang libclang-dev llvm-dev
```

Then:

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

The server runs as a systemd service on `salsa.oso.chalmers.se`. Deploys happen
automatically on every push to `main` via GitHub Actions. A manual deploy can be
triggered from the Actions tab using the "Deploy to SALSA webserver" workflow dispatch.

View logs:
```bash
sudo journalctl -u salsa -f -p warning
```

Restart after config changes:
```bash
sudo systemctl restart salsa
```

### Systemd service

The service file lives at `/etc/systemd/system/salsa.service`:

```ini
[Unit]
Description=Salsa
After=network.target

[Service]
User=salsa
WorkingDirectory=/home/salsa/bin
ExecStart=/home/salsa/bin/target/release/salsa --port 443 --log-to-journald
AmbientCapabilities=CAP_NET_BIND_SERVICE CAP_NET_RAW
CapabilityBoundingSet=CAP_NET_BIND_SERVICE CAP_NET_RAW
Restart=always
EnvironmentFile=/home/salsa/bin/env

[Install]
WantedBy=multi-user.target
```

### TLS certificate

Obtain a certificate via certbot (standalone mode — no web server needed):

```bash
sudo certbot certonly --standalone -d salsa.oso.chalmers.se
```

To ensure the certificate auto-renews every 90 days, add pre/post hooks that
stop and start the service. In `/etc/letsencrypt/renewal/salsa.oso.chalmers.se.conf`:

```
pre_hook = systemctl stop salsa.service
post_hook = systemctl start salsa.service
```

### Initial server setup (WIP)

Steps to set up the service on a fresh Linux machine (work in progress):

- Install Rust: https://rust-lang.org/tools/install
- Install build dependencies: `sudo apt install libuhd-dev libuhd4.6.0 clang libclang-dev llvm-dev`
- Create a `salsa` user and a `githubrunner` user
- Create a `salsaowners` group and add both users to it
- Install the GitHub Actions runner daemon as the `githubrunner` user
- Create the deployment directory `/home/salsa/bin` owned by `salsa:salsaowners`
- Create the systemd service file (see above) and run `sudo systemctl enable salsa`
- Set up the TLS certificate (see above)
- Place `config.toml` and `.secrets.toml` in the config directory

## Architecture

- **Routes** (`src/routes/`) — one file per feature area, registered in `app.rs`
- **Models** (`src/models/`) — database models and telescope abstraction (`SalsaTelescope` / `FakeTelescope`)
- **AppState** — shared state: database connection, telescope handles, config, TLE and weather caches
- **Background tasks** — TLE satellite data refresh, weather cache refresh, booking monitor
- **Database** — SQLite with [Refinery](https://github.com/rust-db/refinery) migrations in `src/database.rs`
