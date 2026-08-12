#!/usr/bin/env python3
"""Plot average received power against time from the SALSA archive.

Reads the SALSA SQLite database directly and renders a self-contained HTML
page — no dependencies beyond the Python standard library, and nothing is
written to the database.

Nothing to install: copy this file anywhere readable by the user that will
run it and invoke it with python3. On the observatory host that means a
world-readable directory such as /tmp, because it is run as the salsa user
(which owns the database) from an account whose home directory it cannot
read.

Selecting Sun as the target puts the receiver in Raw mode, which records
total received power, so the mean across channels is a solar light curve in
arbitrary (but consistent) units. That makes it useful for watching an
eclipse: the absolute scale means nothing, the shape of the dip is the point.

Typical use during an eclipse, on the machine holding the database:

    sudo -u salsa python3 power_vs_time.py --serve 8000

then open http://localhost:8000 (over an SSH tunnel if you are remote). The
page re-queries and redraws itself every 15 s.

Or publish it on the SALSA site itself. The app serves its `assets/` directory
as a fallback, so a file written there is reachable at the site root over the
existing TLS — no second web server, no tunnel, and it works from a phone:

    sudo -u salsa python3 power_vs_time.py \
        --out /path/to/salsa/assets/eclipse.html --follow 30

    → https://salsa.oso.chalmers.se/eclipse.html

Note that this makes the plot public to anyone with the URL. The page is
replaced atomically, so a reader never catches it half-written.

One-shot to a file instead:

    sudo -u salsa python3 power_vs_time.py --out eclipse.html

Numbers rather than a picture:

    sudo -u salsa python3 power_vs_time.py --csv > eclipse.csv
"""

import argparse
import json
import os
import sqlite3
import statistics
import sys
import tempfile
import time
from datetime import datetime, timedelta, timezone

try:
    from zoneinfo import ZoneInfo
except ImportError:  # Python < 3.9
    ZoneInfo = None

DEFAULT_DB = "/home/salsa/data/database.sqlite3"
LINE_COLOR = "#1d4ed8"
INK = "#1a1d1f"
MUTED = "#4b5258"
GRID = "#e3e7ea"
AXIS = "#9ca3af"
SURFACE = "#f9fafb"


# ── database ────────────────────────────────────────────────────────────────


def connect(path):
    """Open the archive without ever writing to it.

    The database runs in WAL mode, and a plain read-only open needs write
    access to the -shm sidecar, which fails for anyone but the salsa user.
    Fall back to a normal connection pinned read-only with query_only, which
    works wherever the file itself is readable.
    """
    if not os.path.exists(path):
        raise SystemExit(
            f"No database at {path}. Point --db at the SALSA archive, "
            f"e.g. --db {DEFAULT_DB}"
        )
    try:
        conn = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
        conn.execute("SELECT 1 FROM observation LIMIT 1")
        return conn
    except sqlite3.OperationalError:
        try:
            conn = sqlite3.connect(path)
            conn.execute("PRAGMA query_only = ON")
            conn.execute("SELECT 1 FROM observation LIMIT 1")
            return conn
        except sqlite3.OperationalError as err:
            raise SystemExit(
                f"Could not read {path}: {err}. If this is the live archive, "
                f"run as the owning user: sudo -u salsa python3 {os.path.basename(__file__)} ..."
            )


def fetch(conn, telescope, target, since_unix, until_unix, stat, channels):
    """Return [(unix_start, power, elevation_deg, mode)] oldest first."""
    rows = conn.execute(
        """SELECT start_time, target_y, observation_mode, amplitudes_json
             FROM observation
            WHERE telescope_id = ?
              AND coordinate_system = ?
              AND start_time >= ?
              AND start_time <= ?
         ORDER BY start_time""",
        (telescope, target, since_unix, until_unix),
    ).fetchall()

    points = []
    for start_time, elevation, mode, amplitudes_json in rows:
        try:
            amps = json.loads(amplitudes_json)
        except (TypeError, ValueError):
            continue
        if channels:
            lo, hi = channels
            amps = amps[lo:hi]
        amps = [a for a in amps if isinstance(a, (int, float))]
        if not amps:
            continue
        value = statistics.median(amps) if stat == "median" else sum(amps) / len(amps)
        points.append((start_time, value, elevation, mode or "?"))
    return points


# ── chart ───────────────────────────────────────────────────────────────────


def nice_step(span, target_divisions):
    """A round step of roughly span/target_divisions: 1, 2 or 5 times a power of ten."""
    if span <= 0:
        return 1.0
    raw = span / max(1, target_divisions)
    magnitude = 10 ** (len(str(int(abs(raw)))) - 1) if abs(raw) >= 1 else 1.0
    while magnitude > abs(raw):
        magnitude /= 10.0
    for multiple in (1, 2, 5, 10):
        if magnitude * multiple >= raw:
            return magnitude * multiple
    return magnitude * 10


def time_ticks(t0, t1, tz):
    """Tick positions on a wall-clock-friendly interval across [t0, t1]."""
    span = max(1, t1 - t0)
    for step in (60, 120, 300, 600, 900, 1800, 3600, 7200, 14400):
        if span / step <= 9:
            break
    # Align to the wall clock rather than to the first sample.
    first = datetime.fromtimestamp(t0, tz).replace(second=0, microsecond=0)
    start = int(first.timestamp())
    start -= start % step
    ticks = []
    tick = start
    while tick <= t1:
        if tick >= t0:
            ticks.append(tick)
        tick += step
    if not ticks:
        # A span shorter than one tick interval — the first few observations
        # of the night. Label the data itself rather than nothing.
        ticks = [t0] if t1 == t0 else [t0, t1]
    return ticks


def build_svg(points, tz, relative, stat):
    width, height = 940.0, 430.0
    m_left, m_right, m_top, m_bottom = 78.0, 24.0, 30.0, 52.0
    plot_w = width - m_left - m_right
    plot_h = height - m_top - m_bottom
    plot_right, plot_bottom = m_left + plot_w, m_top + plot_h

    times = [p[0] for p in points]
    values = [p[1] for p in points]
    t0, t1 = min(times), max(times)
    single_instant = t1 == t0
    # One observation, or a dead-flat run, spans nothing on either axis. Give
    # each a band scaled to the value rather than an arbitrary ±1, which for
    # power around 1e-3 drew the point pinned to a meaningless axis.
    v_lo, v_hi = min(values), max(values)
    if v_hi - v_lo < 1e-12:
        margin = abs(v_hi) * 0.05 or 1.0
        v_lo, v_hi = v_lo - margin, v_hi + margin
    pad = (v_hi - v_lo) * 0.08
    v_lo, v_hi = v_lo - pad, v_hi + pad

    def x_for(t):
        if single_instant:
            return m_left + plot_w / 2
        return m_left + ((t - t0) / (t1 - t0)) * plot_w

    def y_for(v):
        return m_top + (v_hi - v) / (v_hi - v_lo) * plot_h

    parts = [
        f'<rect x="{m_left:.1f}" y="{m_top:.1f}" width="{plot_w:.1f}" '
        f'height="{plot_h:.1f}" fill="{SURFACE}"/>'
    ]

    y_step = nice_step(v_hi - v_lo, 5)
    tick = (int(v_lo / y_step)) * y_step
    while tick <= v_hi:
        if tick >= v_lo:
            y = y_for(tick)
            label = f"{tick:.1f}%" if relative else f"{tick:.4g}"
            parts.append(
                f'<line x1="{m_left:.1f}" y1="{y:.1f}" x2="{plot_right:.1f}" '
                f'y2="{y:.1f}" stroke="{GRID}"/>'
                f'<text x="{m_left - 8:.1f}" y="{y:.1f}" text-anchor="end" '
                f'alignment-baseline="middle" font-size="11" fill="{MUTED}">{label}</text>'
            )
        tick += y_step

    for tick in time_ticks(t0, t1, tz):
        x = x_for(tick)
        label = datetime.fromtimestamp(tick, tz).strftime("%H:%M")
        parts.append(
            f'<line x1="{x:.1f}" y1="{plot_bottom:.1f}" x2="{x:.1f}" '
            f'y2="{plot_bottom + 4:.1f}" stroke="{AXIS}"/>'
            f'<text x="{x:.1f}" y="{plot_bottom + 18:.1f}" text-anchor="middle" '
            f'font-size="11" fill="{MUTED}">{label}</text>'
        )

    parts.append(
        f'<line x1="{m_left:.1f}" y1="{m_top:.1f}" x2="{m_left:.1f}" '
        f'y2="{plot_bottom:.1f}" stroke="{AXIS}"/>'
        f'<line x1="{m_left:.1f}" y1="{plot_bottom:.1f}" x2="{plot_right:.1f}" '
        f'y2="{plot_bottom:.1f}" stroke="{AXIS}"/>'
    )

    path = " ".join(
        f"{'M' if i == 0 else 'L'} {x_for(t):.2f} {y_for(v):.2f}"
        for i, (t, v) in enumerate(zip(times, values))
    )
    parts.append(f'<path d="{path}" fill="none" stroke="{LINE_COLOR}" stroke-width="1.8"/>')
    # Individual integrations are worth seeing while they are still countable.
    if len(points) <= 150:
        for t, v in zip(times, values):
            parts.append(
                f'<circle cx="{x_for(t):.2f}" cy="{y_for(v):.2f}" r="2.4" fill="{LINE_COLOR}"/>'
            )

    unit = "% of median" if relative else "arb. units"
    y_title_y = m_top + plot_h / 2
    parts.append(
        f'<text x="16" y="{y_title_y:.1f}" text-anchor="middle" font-size="12" '
        f'fill="{MUTED}" transform="rotate(-90 16 {y_title_y:.1f})">'
        f"{stat.capitalize()} power ({unit})</text>"
        f'<text x="{m_left + plot_w / 2:.1f}" y="{height - 6:.1f}" text-anchor="middle" '
        f'font-size="12" fill="{MUTED}">Time ({tz_label(tz)})</text>'
    )

    parts.append(
        f'<g class="cursor" visibility="hidden" pointer-events="none">'
        f'<line class="cursor-line" y1="{m_top:.1f}" y2="{plot_bottom:.1f}" '
        f'stroke="{MUTED}" stroke-dasharray="3,3"/>'
        f'<circle class="cursor-dot" r="4" fill="{LINE_COLOR}"/>'
        f'<text class="cursor-text" y="{m_top + 14:.1f}" font-size="12" '
        f'font-weight="600" fill="{INK}"/></g>'
        f'<rect class="capture" x="{m_left:.1f}" y="{m_top:.1f}" width="{plot_w:.1f}" '
        f'height="{plot_h:.1f}" fill="transparent"/>'
    )

    samples = [
        {
            "x": round(x_for(t), 2),
            "y": round(y_for(v), 2),
            "label": datetime.fromtimestamp(t, tz).strftime("%H:%M:%S"),
            "value": f"{v:.2f}%" if relative else f"{v:.4g}",
            "el": f"{el:.1f}",
        }
        for (t, v, el, _mode) in points
    ]
    svg = (
        f'<svg viewBox="0 0 {width:.0f} {height:.0f}" '
        f'xmlns="http://www.w3.org/2000/svg" style="max-width:100%;height:auto;">'
        + "".join(parts)
        + "</svg>"
    )
    return svg, json.dumps(samples)


def tz_label(tz):
    return getattr(tz, "key", None) or datetime.now(tz).strftime("%Z")


HOVER_JS = """
(function () {
  var svg = document.querySelector('svg');
  if (!svg || !window.SAMPLES || !SAMPLES.length) return;
  var cursor = svg.querySelector('.cursor');
  var line = svg.querySelector('.cursor-line');
  var dot = svg.querySelector('.cursor-dot');
  var text = svg.querySelector('.cursor-text');
  var capture = svg.querySelector('.capture');
  var viewW = svg.viewBox.baseVal.width;
  function show(evt) {
    var rect = svg.getBoundingClientRect();
    var xView = (evt.clientX - rect.left) * viewW / rect.width;
    var best = SAMPLES[0], bestD = Infinity;
    for (var i = 0; i < SAMPLES.length; i++) {
      var d = Math.abs(SAMPLES[i].x - xView);
      if (d < bestD) { bestD = d; best = SAMPLES[i]; }
    }
    line.setAttribute('x1', best.x);
    line.setAttribute('x2', best.x);
    dot.setAttribute('cx', best.x);
    dot.setAttribute('cy', best.y);
    text.textContent = best.label + '  ' + best.value + '  (el ' + best.el + '\\u00b0)';
    var left = best.x < viewW / 2;
    text.setAttribute('x', left ? best.x + 10 : best.x - 10);
    text.setAttribute('text-anchor', left ? 'start' : 'end');
    cursor.setAttribute('visibility', 'visible');
  }
  capture.addEventListener('pointermove', show);
  capture.addEventListener('pointerdown', show);
  capture.addEventListener('pointerleave', function () {
    cursor.setAttribute('visibility', 'hidden');
  });
})();
"""

PAGE = """<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<title>{title}</title>
{refresh}
<style>
 body {{ margin: 0; padding: 24px; background: #eef4f7; color: {ink};
        font-family: system-ui, -apple-system, "Segoe UI", sans-serif; }}
 .card {{ max-width: 1000px; margin: 0 auto; background: #fff; border-radius: 10px;
         padding: 20px 24px 8px; box-shadow: 0 1px 3px rgba(0,0,0,.12); }}
 h1 {{ font-size: 18px; margin: 0 0 4px; }}
 p.sub {{ font-size: 13px; color: {muted}; margin: 0 0 14px; }}
 p.foot {{ font-size: 12px; color: {muted}; margin: 4px 0 12px; }}
</style></head>
<body><div class="card">
<h1>{title}</h1>
<p class="sub">{subtitle}</p>
{body}
<p class="foot">{footer}</p>
</div>
<script>window.SAMPLES = {samples};</script>
<script>{hover}</script>
</body></html>
"""


def render(points, args, tz):
    generated = datetime.now(tz).strftime("%H:%M:%S")
    # Only when something is actually updating the page behind the reader:
    # a one-shot file is usually a keepsake, and a page that reloads itself
    # forever is a nuisance in a saved copy.
    live = args.serve or args.follow
    refresh = f'<meta http-equiv="refresh" content="{args.refresh}">' if live else ""
    title = f"{args.telescope} — {args.target} power vs time"

    since, until = window(args, tz)
    if args.since or args.until:
        span = f"from {datetime.fromtimestamp(since, tz):%H:%M}"
        if args.until:
            span += f" to {datetime.fromtimestamp(until, tz):%H:%M}"
        window_note = f"Window: {span}."
    else:
        window_note = f"Window: the last {args.hours:g} h."

    if not points:
        body = (
            f'<p style="font-size:14px;color:{MUTED}">No matching observations yet. '
            f"Waiting for {args.telescope} to record a {args.target} observation.</p>"
        )
        return PAGE.format(
            title=title,
            refresh=refresh,
            ink=INK,
            muted=MUTED,
            subtitle=window_note,
            body=body,
            footer=f"Generated {generated}. Database: {args.db}",
            samples="[]",
            hover="",
        )

    values = [p[1] for p in points]
    if args.relative:
        baseline = statistics.median(values)
        if baseline:
            points = [(t, 100.0 * v / baseline, el, m) for (t, v, el, m) in points]

    svg, samples = build_svg(points, tz, args.relative, args.stat)
    modes = sorted({p[3] for p in points})
    elevations = [p[2] for p in points]
    first = datetime.fromtimestamp(points[0][0], tz).strftime("%H:%M")
    last = datetime.fromtimestamp(points[-1][0], tz).strftime("%H:%M")
    subtitle = (
        f"{len(points)} observation{'' if len(points) == 1 else 's'}, {first}–{last}, "
        f"elevation {min(elevations):.1f}°–{max(elevations):.1f}°, "
        f"mode {'/'.join(modes)}"
    )
    footer = (
        f"Generated {generated}. {window_note} "
        f"Power is the {args.stat} across channels; "
        f"in Raw mode this is total received power in arbitrary units, so only "
        f"relative changes are meaningful. Database: {args.db}"
    )
    return PAGE.format(
        title=title,
        refresh=refresh,
        ink=INK,
        muted=MUTED,
        subtitle=subtitle,
        body=svg,
        footer=footer,
        samples=samples,
        hover=HOVER_JS,
    )


# ── entry points ────────────────────────────────────────────────────────────


def parse_time(text, tz):
    """Accept 21:30, 2026-08-12, 2026-08-12[T ]21:30[:ss], or unix seconds.

    A bare time means today in `tz`, which is what you want when trimming off
    an afternoon test run from an evening's observing.
    """
    text = text.strip()
    if text.isdigit() and len(text) >= 9:
        return int(text)
    today = datetime.now(tz).date()
    formats = (
        ("%H:%M", True),
        ("%H:%M:%S", True),
        ("%Y-%m-%d", False),
        ("%Y-%m-%d %H:%M", False),
        ("%Y-%m-%d %H:%M:%S", False),
        ("%Y-%m-%dT%H:%M", False),
        ("%Y-%m-%dT%H:%M:%S", False),
    )
    for fmt, time_only in formats:
        try:
            parsed = datetime.strptime(text, fmt)
        except ValueError:
            continue
        if time_only:
            parsed = parsed.replace(year=today.year, month=today.month, day=today.day)
        return int(parsed.replace(tzinfo=tz).timestamp())
    raise SystemExit(
        f"Could not read {text!r} as a time. Try 21:30, 2026-08-12T21:30, or unix seconds."
    )


def window(args, tz):
    """(since, until) as unix seconds. --since pins the start; --hours slides it."""
    if args.since:
        since = parse_time(args.since, tz)
    else:
        since = int((datetime.now(timezone.utc) - timedelta(hours=args.hours)).timestamp())
    # Far enough ahead to mean "no end", without special-casing it in the query.
    until = parse_time(args.until, tz) if args.until else 2**40
    return since, until


def gather(args, tz):
    since, until = window(args, tz)
    conn = connect(args.db)
    try:
        return fetch(
            conn, args.telescope, args.target, since, until, args.stat, args.channels
        )
    finally:
        conn.close()


def write_csv(points, tz):
    out = sys.stdout
    out.write("iso_time,unix_time,power,elevation_deg,mode\n")
    for t, v, el, mode in points:
        iso = datetime.fromtimestamp(t, tz).isoformat()
        out.write(f"{iso},{t},{v!r},{el},{mode}\n")


def write_page(path, text):
    """Replace `path` atomically.

    Written into a webroot, the page is being read while it is rewritten;
    a plain open-and-write would occasionally serve a half-finished file.
    Rename within the same directory is atomic, so a reader sees either the
    old page or the new one. World-readable because whatever serves the
    directory is unlikely to be the user running this.
    """
    directory = os.path.dirname(os.path.abspath(path))
    if not os.path.isdir(directory):
        raise SystemExit(f"No such directory: {directory}")
    try:
        handle, temporary = tempfile.mkstemp(dir=directory, suffix=".tmp")
    except OSError as err:
        raise SystemExit(
            f"Cannot write to {directory}: {err}.\n"
            f"Check the owner with: ls -ld {directory}\n"
            f"A directory under the app's assets tree is often not owned by the "
            f"user that runs the app, so either write into a subdirectory that "
            f"this user does own (mkdir it and chown it once), or run as a user "
            f"that can write here."
        )
    try:
        with os.fdopen(handle, "w", encoding="utf-8") as out:
            out.write(text)
        os.chmod(temporary, 0o644)
        os.replace(temporary, path)
    except BaseException:
        if os.path.exists(temporary):
            os.unlink(temporary)
        raise


def channel_range(text):
    try:
        lo, hi = text.split(":")
        return (int(lo), int(hi))
    except ValueError:
        raise argparse.ArgumentTypeError("channels must look like 100:400")


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--db", default=DEFAULT_DB, help=f"database path (default {DEFAULT_DB})")
    parser.add_argument("--telescope", default="vale", help="telescope_id (default vale)")
    parser.add_argument(
        "--target",
        default="sun",
        help="coordinate_system to filter on: sun, galactic, equatorial, horizontal",
    )
    parser.add_argument(
        "--hours",
        type=float,
        default=12,
        help="look back this many hours from now (a sliding window; default 12)",
    )
    parser.add_argument(
        "--since",
        help="fixed start instead of --hours: 21:30, 2026-08-12T21:30, or unix seconds. "
        "Use this to drop an earlier test run.",
    )
    parser.add_argument("--until", help="fixed end, same formats as --since")
    parser.add_argument(
        "--stat",
        choices=("mean", "median"),
        default="mean",
        help="how to combine channels; median shrugs off narrow RFI",
    )
    parser.add_argument(
        "--channels", type=channel_range, help="restrict to a channel slice, e.g. 100:400"
    )
    parser.add_argument(
        "--relative",
        action="store_true",
        help="plot as %% of the run's median, which makes a small dip readable",
    )
    parser.add_argument("--tz", default="Europe/Stockholm", help="timezone for the time axis")
    parser.add_argument("--out", help="write the page to this file and exit")
    parser.add_argument(
        "--follow",
        type=int,
        metavar="SECONDS",
        help="with --out, keep rewriting the page this often instead of exiting",
    )
    parser.add_argument("--serve", type=int, metavar="PORT", help="serve a self-refreshing page")
    parser.add_argument("--refresh", type=int, default=15, help="seconds between refreshes")
    parser.add_argument("--csv", action="store_true", help="write CSV to stdout instead")
    args = parser.parse_args()

    tz = ZoneInfo(args.tz) if ZoneInfo else timezone.utc

    if args.csv:
        write_csv(gather(args, tz), tz)
        return

    if args.serve:
        from http.server import BaseHTTPRequestHandler, HTTPServer

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                try:
                    page = render(gather(args, tz), args, tz).encode("utf-8")
                except Exception as err:  # keep the page up through a transient DB error
                    page = f"<p>Could not read the archive: {err}</p>".encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "text/html; charset=utf-8")
                self.send_header("Content-Length", str(len(page)))
                self.end_headers()
                self.wfile.write(page)

            def log_message(self, *_args):
                pass

        print(f"Serving on http://localhost:{args.serve} (refreshing every {args.refresh}s)")
        print("Ctrl-C to stop.")
        try:
            HTTPServer(("127.0.0.1", args.serve), Handler).serve_forever()
        except KeyboardInterrupt:
            print("\nStopped.")
        return

    if not args.out:
        sys.stdout.write(render(gather(args, tz), args, tz))
        return

    write_page(args.out, render(gather(args, tz), args, tz))
    if not args.follow:
        print(f"Wrote {args.out}")
        return

    print(f"Updating {args.out} every {args.follow}s. Ctrl-C to stop.")
    try:
        while True:
            time.sleep(args.follow)
            try:
                write_page(args.out, render(gather(args, tz), args, tz))
            except Exception as err:  # a transient DB error must not end the night
                print(f"skipped an update: {err}", file=sys.stderr)
    except KeyboardInterrupt:
        print("\nStopped.")


if __name__ == "__main__":
    main()
