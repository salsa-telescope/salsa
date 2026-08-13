#!/usr/bin/env python3
"""Extract solar observations to CSV, one row per observation, for comparing
runs across days.

Sun observations are recorded in Raw mode, which measures total received
power, so the mean across channels is a light curve in arbitrary units. The
absolute scale drifts between days and means nothing on its own, so each day
is also expressed as a percentage of its own baseline period — that is the
column to compare across days.

Standard library only, and nothing is written to the database.

Two runs, yesterday and today, as one CSV:

    sudo -u salsa python3 solar_csv.py --exclude 1407.7:1408.3 > solar.csv

Pivot the same data into azimuth bins, one column per day, to line runs up by
pointing rather than by clock:

    sudo -u salsa python3 solar_csv.py --by-azimuth 0.5 --exclude 1407.7:1408.3

Be careful what that comparison can settle. Tracking the Sun on consecutive
days, azimuth and clock time are nearly the same question: at Onsala in
mid-August the Sun reaches a given azimuth only ~40 s later from one day to
the next, so a feature fixed on the ground and a feature fixed to the clock
look alike. What does change is elevation — about 0.37° lower at the same
azimuth each day, which is most of a solar diameter. The pivot therefore
carries elevation per day as well: if a dip comes from the Sun grazing the
top of something, that shift should deepen it, and the depth is the
measurement worth comparing. Separating azimuth from time properly needs
runs weeks apart, or a deliberate pointing offset.

A per-day summary is written to stderr, so it stays out of the CSV when you
redirect stdout.
"""

import argparse
import json
import os
import sqlite3
import statistics
import sys
from datetime import datetime, timedelta, timezone

try:
    from zoneinfo import ZoneInfo
except ImportError:  # Python < 3.9
    ZoneInfo = None

DEFAULT_DB = "/home/salsa/data/database.sqlite3"


def connect(path):
    """Open the archive read-only.

    A read-only open of a WAL database needs write access to the -shm
    sidecar, which only the owning user has; fall back to a normal connection
    pinned with query_only, which cannot write either.
    """
    if not os.path.exists(path):
        raise SystemExit(f"No database at {path}. Point --db at the SALSA archive.")
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
                f"Could not read {path}: {err}. If this is the live archive, run as "
                f"the owning user: sudo -u salsa python3 {os.path.basename(__file__)} ..."
            )


def mhz_range(text):
    try:
        lo, hi = text.split(":")
        lo, hi = float(lo) * 1e6, float(hi) * 1e6
    except ValueError:
        raise argparse.ArgumentTypeError("--exclude wants MHZ_LOW:MHZ_HIGH, e.g. 1407.7:1408.3")
    if hi <= lo:
        raise argparse.ArgumentTypeError(f"--exclude {text}: the high edge must be the larger")
    return (lo, hi)


def clock_window(text):
    try:
        start, end = text.split("-")
        return (
            datetime.strptime(start.strip(), "%H:%M").time(),
            datetime.strptime(end.strip(), "%H:%M").time(),
        )
    except ValueError:
        raise argparse.ArgumentTypeError("--baseline-window wants HH:MM-HH:MM, e.g. 15:00-17:00")


def read_observations(conn, args, tz):
    """One record per observation, oldest first, across every requested day."""
    days = sorted(args.day)
    first = datetime.combine(days[0], datetime.min.time(), tz)
    last = datetime.combine(days[-1] + timedelta(days=1), datetime.min.time(), tz)

    rows = conn.execute(
        """SELECT start_time, target_x, target_y, observation_mode, telescope_id,
                  frequencies_json, amplitudes_json
             FROM observation
            WHERE telescope_id = ?
              AND coordinate_system = 'sun'
              AND start_time >= ?
              AND start_time < ?
         ORDER BY start_time""",
        (args.telescope, int(first.timestamp()), int(last.timestamp())),
    ).fetchall()

    freq_cache = {}
    records = []
    for start_time, azimuth, elevation, mode, telescope, freqs_json, amps_json in rows:
        when = datetime.fromtimestamp(start_time, tz)
        if when.date() not in args.day:
            continue
        try:
            amps = json.loads(amps_json)
        except (TypeError, ValueError):
            continue
        if args.exclude:
            if freqs_json not in freq_cache:
                try:
                    freq_cache[freqs_json] = json.loads(freqs_json)
                except (TypeError, ValueError):
                    freq_cache[freqs_json] = []
            freqs = freq_cache[freqs_json]
            if freqs and len(freqs) == len(amps):
                amps = [
                    a
                    for a, f in zip(amps, freqs)
                    if not any(lo <= f <= hi for lo, hi in args.exclude)
                ]
        amps = [a for a in amps if isinstance(a, (int, float))]
        # An all-zero spectrum is a buffer a run was stopped before it filled,
        # not a measurement. Releases before v1.5.3 filed those as 0 s
        # observations, so older stretches of the archive still hold them.
        if not amps or not any(amps):
            continue
        power = statistics.median(amps) if args.stat == "median" else sum(amps) / len(amps)
        records.append(
            {
                "date": when.date(),
                "when": when,
                "unix": start_time,
                "az": azimuth,
                "el": elevation,
                "power": power,
                "channels": len(amps),
                "mode": mode or "?",
                "telescope": telescope,
            }
        )
    return records


def add_baselines(records, args, tz):
    """Give every record a percentage of its own day's baseline.

    Comparing days needs a per-day reference: the absolute scale drifts with
    receiver gain and with the Sun's own activity, so only the shape survives
    from one day to the next.
    """
    summaries = []
    for date in sorted({r["date"] for r in records}):
        day = [r for r in records if r["date"] == date]
        if args.baseline_window:
            start, end = args.baseline_window
            reference = [r for r in day if start <= r["when"].time() <= end]
            described = f"{start:%H:%M}–{end:%H:%M}"
        else:
            cutoff = day[0]["when"] + timedelta(minutes=args.baseline_minutes)
            reference = [r for r in day if r["when"] <= cutoff]
            described = f"first {args.baseline_minutes:g} min"
        if not reference:
            reference, described = day, "all of that day (baseline window was empty)"
        level = statistics.median([r["power"] for r in reference])
        for r in day:
            r["pct"] = 100.0 * r["power"] / level if level else float("nan")
        summaries.append(
            {
                "date": date,
                "n": len(day),
                "from": day[0]["when"],
                "to": day[-1]["when"],
                "az": (min(r["az"] for r in day), max(r["az"] for r in day)),
                "el": (min(r["el"] for r in day), max(r["el"] for r in day)),
                "baseline": level,
                "baseline_n": len(reference),
                "baseline_described": described,
            }
        )
    return summaries


def write_summary(summaries, args):
    out = sys.stderr
    out.write(f"{args.telescope}: {len(summaries)} day(s)\n")
    for s in summaries:
        out.write(
            f"  {s['date']}  {s['n']:4d} obs  {s['from']:%H:%M}–{s['to']:%H:%M}  "
            f"az {s['az'][0]:.1f}–{s['az'][1]:.1f}  el {s['el'][0]:.1f}–{s['el'][1]:.1f}\n"
            f"             100% = {s['baseline']:.6g} "
            f"(median of {s['baseline_n']} obs, {s['baseline_described']})\n"
        )
    if args.exclude:
        bands = ", ".join(f"{lo / 1e6:g}–{hi / 1e6:g} MHz" for lo, hi in args.exclude)
        out.write(f"  excluding {bands}\n")


def write_rows(records):
    out = sys.stdout
    out.write(
        "date,iso_time,unix_time,telescope,azimuth_deg,elevation_deg,"
        "power,power_pct,channels,mode\n"
    )
    for r in records:
        out.write(
            f"{r['date']},{r['when'].isoformat()},{r['unix']},{r['telescope']},"
            f"{r['az']:.4f},{r['el']:.4f},{r['power']!r},{r['pct']:.4f},"
            f"{r['channels']},{r['mode']}\n"
        )


def write_azimuth_pivot(records, width):
    """Median power per azimuth bin, one column per day.

    Rows line up by pointing rather than by clock, which is the comparison
    that separates something on the ground from something in the sky.
    """
    dates = sorted({r["date"] for r in records})
    bins = {}
    for r in records:
        bins.setdefault(round(r["az"] / width) * width, {}).setdefault(r["date"], []).append(r)

    out = sys.stdout
    header = ["azimuth_deg"]
    for d in dates:
        header += [f"power_pct_{d}", f"elevation_deg_{d}", f"n_{d}"]
    out.write(",".join(header) + "\n")
    for az in sorted(bins):
        cells = [f"{az:.2f}"]
        for d in dates:
            group = bins[az].get(d)
            if group:
                cells += [
                    f"{statistics.median(r['pct'] for r in group):.4f}",
                    f"{statistics.median(r['el'] for r in group):.4f}",
                    str(len(group)),
                ]
            else:
                cells += ["", "", "0"]
        out.write(",".join(cells) + "\n")


def parse_day(text, tz):
    if text == "today":
        return datetime.now(tz).date()
    if text == "yesterday":
        return datetime.now(tz).date() - timedelta(days=1)
    try:
        return datetime.strptime(text, "%Y-%m-%d").date()
    except ValueError:
        raise SystemExit(f"Could not read {text!r} as a date. Use YYYY-MM-DD, today or yesterday.")


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--db", default=DEFAULT_DB, help=f"database path (default {DEFAULT_DB})")
    parser.add_argument("--telescope", default="vale", help="telescope_id (default vale)")
    parser.add_argument(
        "--day",
        action="append",
        help="a local date to include: YYYY-MM-DD, today or yesterday. "
        "Repeatable; defaults to yesterday and today.",
    )
    parser.add_argument(
        "--stat", choices=("mean", "median"), default="mean", help="how to combine channels"
    )
    parser.add_argument(
        "--exclude",
        type=mhz_range,
        action="append",
        metavar="MHZ_LOW:MHZ_HIGH",
        help="drop an interfering band before averaging. Repeatable.",
    )
    parser.add_argument(
        "--baseline-minutes",
        type=float,
        default=60,
        help="per-day 100%% level from the first N minutes of that day (default 60)",
    )
    parser.add_argument(
        "--baseline-window",
        type=clock_window,
        metavar="HH:MM-HH:MM",
        help="per-day 100%% level from this clock window instead, which keeps the "
        "reference at the same time of day when the runs start at different hours",
    )
    parser.add_argument(
        "--by-azimuth",
        type=float,
        metavar="DEGREES",
        help="pivot into azimuth bins of this width, one column per day",
    )
    parser.add_argument("--tz", default="Europe/Stockholm", help="timezone for dates and times")
    args = parser.parse_args()

    tz = ZoneInfo(args.tz) if ZoneInfo else timezone.utc
    if args.day:
        args.day = [parse_day(d, tz) for d in args.day]
    else:
        today = datetime.now(tz).date()
        args.day = [today - timedelta(days=1), today]

    conn = connect(args.db)
    try:
        records = read_observations(conn, args, tz)
    finally:
        conn.close()

    if not records:
        days = ", ".join(str(d) for d in sorted(args.day))
        raise SystemExit(f"No sun observations for {args.telescope} on {days}.")

    summaries = add_baselines(records, args, tz)
    write_summary(summaries, args)
    if args.by_azimuth:
        write_azimuth_pivot(records, args.by_azimuth)
    else:
        write_rows(records)


if __name__ == "__main__":
    main()
