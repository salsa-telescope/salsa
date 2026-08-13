#!/usr/bin/env python3
"""Interactive plots of exported solar runs, for comparing days.

Reads the CSV written by solar_csv.py and opens a matplotlib window with one
panel per x axis: normalised power against time of day, and against azimuth.
Both days are drawn in the same panel so they can be compared directly.

    python3 solar_plot.py solar.csv

Interaction is matplotlib's own — scroll or use the toolbar to zoom and pan,
and the home button to reset — plus a readout that follows the cursor and
names the nearest observation on either curve.

Panels are chosen with --x, which takes any of time, azimuth and elevation:

    python3 solar_plot.py solar.csv --x time,azimuth,elevation

Time is plotted as time of day, so runs that started at different hours still
overlay. --time HH:MM-HH:MM narrows every day to the same clock window, which
is the way to line two evenings up on the part that matters. Power is the power_pct column, each day against its own baseline;
--absolute plots the raw power instead, which is only worth doing within a
single day since the scale drifts between runs.

Needs matplotlib. Everything else is the standard library.

    pip install matplotlib
"""

import argparse
import csv
import sys
from collections import OrderedDict
from datetime import datetime

try:
    import matplotlib.pyplot as plt
    from matplotlib.ticker import FuncFormatter
except ImportError:
    sys.exit(
        "This one needs matplotlib:\n"
        "    pip install matplotlib\n"
        "or, without touching the system python:\n"
        "    python3 -m venv ~/venv && ~/venv/bin/pip install matplotlib\n"
        "    ~/venv/bin/python solar_plot.py solar.csv"
    )

# Fixed order, so a day keeps its colour however many are plotted. The first
# two are far apart under simulated colour blindness as well as normal vision.
SERIES_COLORS = ["#1d4ed8", "#be185d", "#9a6a07", "#0f766e", "#7c3aed"]
INK = "#1a1d1f"
MUTED = "#4b5258"
GRID = "#e3e7ea"

AXES = {
    "time": ("Time of day", "hours"),
    "azimuth": ("Azimuth (°)", "azimuth_deg"),
    "elevation": ("Elevation (°)", "elevation_deg"),
}


def read_csv(path):
    """Group the exported rows by date, oldest first within each day."""
    days = OrderedDict()
    with open(path, newline="", encoding="utf-8") as handle:
        reader = csv.DictReader(handle)
        missing = {"date", "iso_time", "azimuth_deg", "elevation_deg", "power"} - set(
            reader.fieldnames or []
        )
        if missing:
            sys.exit(
                f"{path} does not look like a solar_csv.py export: "
                f"missing column(s) {', '.join(sorted(missing))}"
            )
        for row in reader:
            when = datetime.fromisoformat(row["iso_time"])
            point = {
                "when": when,
                "hours": when.hour + when.minute / 60 + when.second / 3600,
                "azimuth_deg": float(row["azimuth_deg"]),
                "elevation_deg": float(row["elevation_deg"]),
                "power": float(row["power"]),
                "power_pct": float(row["power_pct"]) if row.get("power_pct") else float("nan"),
            }
            days.setdefault(row["date"], []).append(point)
    for points in days.values():
        points.sort(key=lambda p: p["when"])
    return days


def clock_window(text):
    try:
        start, end = text.split("-")
        return (
            datetime.strptime(start.strip(), "%H:%M").time(),
            datetime.strptime(end.strip(), "%H:%M").time(),
        )
    except ValueError:
        raise argparse.ArgumentTypeError("--time wants HH:MM-HH:MM, e.g. 17:00-20:00")


def within(when, window):
    """Whether a timestamp falls inside a clock window, midnight-safe."""
    start, end = window
    clock = when.time()
    if start <= end:
        return start <= clock <= end
    return clock >= start or clock <= end


def hhmm(hours, _pos=None):
    hours = hours % 24
    return f"{int(hours):02d}:{int(round((hours % 1) * 60)) % 60:02d}"


def add_readout(figure, panels):
    """A cursor readout naming the nearest observation on any curve.

    `panels` is [(axes, [(date, xs, ys, points)])]. Nearest is measured in
    display coordinates so it behaves the same whatever the zoom.
    """
    notes = {}
    for axes, _series in panels:
        note = axes.annotate(
            "",
            xy=(0, 0),
            xytext=(12, 14),
            textcoords="offset points",
            bbox={"boxstyle": "round,pad=0.4", "fc": "white", "ec": "#9ca3af", "alpha": 0.95},
            fontsize=9,
            color=INK,
            zorder=10,
        )
        note.set_visible(False)
        notes[axes] = note

    def on_move(event):
        redraw = False
        for axes, series in panels:
            note = notes[axes]
            if event.inaxes is not axes:
                if note.get_visible():
                    note.set_visible(False)
                    redraw = True
                continue
            best = None
            for date, xs, ys, points in series:
                for x, y, point in zip(xs, ys, points):
                    px, py = axes.transData.transform((x, y))
                    distance = (px - event.x) ** 2 + (py - event.y) ** 2
                    if best is None or distance < best[0]:
                        best = (distance, x, y, date, point)
            if best is None or best[0] > 40**2:
                if note.get_visible():
                    note.set_visible(False)
                    redraw = True
                continue
            _distance, x, y, date, point = best
            note.xy = (x, y)
            note.set_text(
                f"{date} {point['when']:%H:%M:%S}\n"
                f"{y:.2f}    az {point['azimuth_deg']:.1f}°  el {point['elevation_deg']:.1f}°"
            )
            note.set_visible(True)
            redraw = True
        if redraw:
            figure.canvas.draw_idle()

    figure.canvas.mpl_connect("motion_notify_event", on_move)


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("csv", help="a CSV written by solar_csv.py")
    parser.add_argument(
        "--x",
        default="time,azimuth",
        help="comma-separated panels: time, azimuth, elevation (default time,azimuth)",
    )
    parser.add_argument(
        "--absolute",
        action="store_true",
        help="plot raw power rather than each day's percentage of its own baseline",
    )
    parser.add_argument(
        "--time",
        type=clock_window,
        metavar="HH:MM-HH:MM",
        help="keep only observations inside this clock window, applied to every day. "
        "The percentages keep the baseline they were exported with, so narrowing "
        "the view does not move the 100%% line.",
    )
    parser.add_argument("--save", metavar="FILE", help="write the figure here as well")
    parser.add_argument(
        "--no-show", action="store_true", help="skip the window; useful with --save"
    )
    args = parser.parse_args()

    wanted = [name.strip() for name in args.x.split(",") if name.strip()]
    unknown = [name for name in wanted if name not in AXES]
    if unknown:
        sys.exit(f"Unknown panel(s): {', '.join(unknown)}. Choose from {', '.join(AXES)}.")
    if not wanted:
        sys.exit("--x needs at least one of: " + ", ".join(AXES))

    days = read_csv(args.csv)
    if not days:
        sys.exit(f"No rows in {args.csv}.")

    if args.time:
        days = OrderedDict(
            (date, kept)
            for date, points in days.items()
            if (kept := [p for p in points if within(p["when"], args.time)])
        )
        if not days:
            start, end = args.time
            sys.exit(f"No observations between {start:%H:%M} and {end:%H:%M} on any day.")

    value_key = "power" if args.absolute else "power_pct"
    y_label = "Power (arb. units)" if args.absolute else "Power (% of each day's baseline)"

    figure, axes_list = plt.subplots(
        len(wanted), 1, figsize=(11, 3.4 * len(wanted) + 0.6), squeeze=False
    )
    axes_list = [row[0] for row in axes_list]
    figure.canvas.manager.set_window_title("SALSA — solar runs")

    panels = []
    for axes, name in zip(axes_list, wanted):
        label, key = AXES[name]
        series = []
        for index, (date, points) in enumerate(days.items()):
            xs = [p[key] for p in points]
            ys = [p[value_key] for p in points]
            colour = SERIES_COLORS[index % len(SERIES_COLORS)]
            axes.plot(xs, ys, "-", lw=1.4, color=colour, label=date, zorder=3)
            axes.plot(xs, ys, ".", ms=3.2, color=colour, zorder=4)
            series.append((date, xs, ys, points))
        panels.append((axes, series))

        axes.set_xlabel(label, color=MUTED)
        axes.set_ylabel(y_label, color=MUTED)
        axes.grid(True, color=GRID, lw=0.8)
        axes.set_axisbelow(True)
        for spine in ("top", "right"):
            axes.spines[spine].set_visible(False)
        axes.tick_params(colors=MUTED, labelsize=9)
        if name == "time":
            axes.xaxis.set_major_formatter(FuncFormatter(hhmm))
        # Two or more days need naming; one is already named by the title.
        if len(days) > 1:
            axes.legend(frameon=False, fontsize=9, labelcolor=MUTED)

    window = ""
    if args.time:
        start, end = args.time
        window = f"    {start:%H:%M}–{end:%H:%M}"
    axes_list[0].set_title(
        f"{' · '.join(days)}    {sum(len(p) for p in days.values())} observations{window}",
        color=INK,
        fontsize=11,
        loc="left",
    )
    add_readout(figure, panels)
    figure.tight_layout()

    if args.save:
        figure.savefig(args.save, dpi=140)
        print(f"Wrote {args.save}")
    if not args.no_show:
        plt.show()


if __name__ == "__main__":
    main()
