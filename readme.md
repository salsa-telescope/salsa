# SALSA

Web-based control system for the [SALSA radio telescopes](https://salsa.oso.chalmers.se)
at Onsala Space Observatory. Users book time slots, point telescopes at targets,
record spectra, and download observation data.

Built with Rust, [Axum](https://github.com/tokio-rs/axum), [Askama](https://github.com/djc/askama)
templates, [HTMX](https://htmx.org), and [Tailwind CSS](https://tailwindcss.com).

## Building

These instructions are for a development machine that compiles SALSA
locally. The production server does not build — it runs a pre-compiled
binary produced by CI (see [Deployment](#deployment) below); its
dependencies are listed separately there.

On Debian/Ubuntu, install the build dependencies first:

```bash
sudo apt install libuhd-dev libuhd4.6.0 clang libclang-dev llvm-dev
```

`libuhd-dev` and the clang/LLVM packages are needed for the UHD
bindings (`uhd-usrp-sys`, which uses `bindgen`). `libuhd4.6.0` is
the runtime library — it is also a transitive dependency of
`libuhd-dev`, so installing the dev package alone pulls it in.

Then:

```bash
cargo build --release
```

The `release` option is required for the code to be efficient enough to handle the 
data streams from the USRP N210 samplers.

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
cargo run -- --config-dir config/ --database-dir data/
```

- `--config-dir` — directory containing `config.toml` and `.secrets.toml`
- `--database-dir` — directory where the SQLite database will be stored

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
To update the version runnnig create a release in github (https://github.com/salsa-telescope/salsa/releases/new), this will automatically be deployed to the machine.

View logs:
```bash
sudo journalctl -u salsa -f -p warning
```

Restart after config changes:
```bash
sudo systemctl restart salsa
```

### Systemd service

The unit file lives at `/etc/systemd/system/salsa.service`. The template
in [`deploy/systemd/salsa.service`](deploy/systemd/salsa.service) is the
exact contents — copy it across on a fresh install:

```bash
sudo cp deploy/systemd/salsa.service /etc/systemd/system/salsa.service
sudo systemctl daemon-reload
sudo systemctl enable salsa
```

The unit hardcodes the production hostname (cert paths under
`/etc/letsencrypt/live/salsa.oso.chalmers.se/`). If you ever deploy on
another host, edit the `KEY_FILE_PATH` / `CERT_FILE_PATH` lines.

This template is not pushed to the server by the deploy workflow; it
only documents what the unit file should look like. Changes to it need
to be applied manually with `sudo systemctl daemon-reload && sudo
systemctl restart salsa` after editing the unit on the host.

### Kernel UDP buffer sizes

UHD streams USRP samples over UDP and needs the kernel's per-socket
buffer caps bumped above the default 200 KB, otherwise packets are
dropped under high-throughput interferometry streams. Copy
[`deploy/sysctl.d/zz-salsa.conf`](deploy/sysctl.d/zz-salsa.conf) to
`/etc/sysctl.d/zz-salsa.conf` and apply with `sudo sysctl --system`
(or just reboot — `systemd-sysctl.service` re-applies it at boot).

The `zz-` prefix is intentional: `libuhd-dev` ships
`/etc/sysctl.d/uhd-usrp2.conf` with smaller defaults that would
otherwise override ours (sysctl files load in lexical order and the
later file wins). Keep the `zz-` prefix on any rename.

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

Steps to set up the service on a fresh Linux machine (work in progress).
The production host runs a pre-compiled binary produced by CI, so it
does not need Rust, the UHD headers, or any build tooling — only the
UHD runtime library:

- Install runtime dependencies: `sudo apt install libuhd4.6.0` (optionally `uhd-host` for the `uhd_find_devices` debug CLI)
- Create a `salsa` user and a `githubrunner` user
- Create a `salsaowners` group and add both users to it
- Install the GitHub Actions runner daemon as the `githubrunner` user
- Create the deployment directory `/home/salsa/bin` owned by `salsa:salsaowners` with mode `2775`, and `chgrp salsaowners /home/salsa` so the runner can traverse into it
- Create `/home/salsa/config` (`salsa:salsa`, `700`) and `/home/salsa/data` (`salsa:salsa`, `700`)
- Install the systemd unit file (see above) and run `sudo systemctl enable salsa`
- Set up the TLS certificate (see above)
- Place `config.toml` and `.secrets.toml` in the config directory (mode `600`, owner `salsa:salsa`)

## Architecture

- **Routes** (`src/routes/`) — one file per feature area, registered in `app.rs`
- **Models** (`src/models/`) — database models and telescope abstraction (`SalsaTelescope` / `FakeTelescope`)
- **AppState** — shared state: database connection, telescope handles, config, TLE and weather caches
- **Background tasks** — TLE satellite data refresh, weather cache refresh, booking monitor
- **Database** — SQLite with [Refinery](https://github.com/rust-db/refinery) migrations in `src/database.rs`
