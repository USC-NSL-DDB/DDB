#!/usr/bin/env python3
"""Turn faketime_pause samples into the perceived time gap per debugger pause.

Each sample is a pair of timestamps taken back to back by the application:

    perceived_us  CLOCK_REALTIME -- what the app thinks the time is. libfaketime
                  subtracts DDB's offset from it, so this is the timeline the app
                  actually experiences.
    real_us       rdtsc -- ground truth. Keeps advancing while the process is
                  stopped by the debugger, and libfaketime cannot touch it.

A pause is a sample-to-sample jump in `real_us` far larger than the normal
sampling interval: real time ran on while the app was frozen and produced nothing.
The question this script answers is what `perceived_us` did across that same jump.

    perceived gap = perceived_us[k] - perceived_us[k-1] - (one sampling interval)

That is the residual: the amount of wall-clock time the pause still leaked into
the application's view of the world after DDB's correction. Without faketime it is
the whole pause. With faketime and a dynamically adjusted offset it should be
close to zero, and how close is the result of the experiment.

Usage:
    ./analyze.py [--pause-ms 100] [--out-dir results] samples.csv [more.csv ...]
"""

import argparse
import csv
import os
import statistics
import sys

# build_all.sh installs matplotlib into ./.pydeps next to this script rather than
# into $HOME, because the root filesystem on the machines this ships with is small
# and usually close to full. Pick it up before anything tries to import it.
_PYDEPS = os.path.join(os.path.dirname(os.path.abspath(__file__)), ".pydeps")
if os.path.isdir(_PYDEPS) and _PYDEPS not in sys.path:
    sys.path.insert(0, _PYDEPS)

# Categorical slots 1 and 2 of the reference palette, used in fixed order.
# Assigned to the series, never to their rank.
SERIES_COLORS = ["#2a78d6", "#1baf7a"]  # blue, aqua
INK_PRIMARY = "#0b0b0b"
INK_SECONDARY = "#52514e"
INK_MUTED = "#8a8985"


def load(path):
    """Read the CSV and rebase both clocks to zero at the first sample."""
    perceived, real = [], []
    with open(path, newline="") as fh:
        for row in csv.DictReader(fh):
            try:
                perceived.append(int(row["perceived_us"]))
                real.append(int(row["real_us"]))
            except (KeyError, ValueError):
                continue  # a torn final line if the app was killed mid-write
    if len(perceived) < 100:
        sys.exit(f"error: {path} has only {len(perceived)} usable samples")
    p0, r0 = perceived[0], real[0]
    return [p - p0 for p in perceived], [r - r0 for r in real]


def analyze(path, pause_ms):
    perceived, real = load(path)

    d_real = [real[i] - real[i - 1] for i in range(1, len(real))]
    d_perc = [perceived[i] - perceived[i - 1] for i in range(1, len(perceived))]

    # What one undisturbed iteration costs. The median is taken over every gap
    # including the pauses, but pauses are a tiny minority of samples and are
    # enormous, so they cannot move a median.
    interval = statistics.median(d_real)

    # A pause is orders of magnitude larger than an iteration. Require it to beat
    # both a multiple of the sampling interval (so a slow iteration or a scheduler
    # hiccup is not mistaken for a pause) and half the pause we asked for (so a
    # long tail of normal jitter cannot qualify).
    threshold = max(10 * interval, 0.5 * pause_ms * 1000)

    pauses = []
    for i, dr in enumerate(d_real):
        if dr <= threshold:
            continue
        dp = d_perc[i]
        pauses.append(
            {
                "real_gap": dr,             # real time across the pause
                "perceived_gap": dp,        # what the app saw across it
                "pause": dr - interval,     # the pause itself, minus the iteration
                "residual": dp - interval,  # <-- the perceived time gap
                "hidden": dr - dp,          # time faketime removed from the app's view
            }
        )

    return {
        "path": path,
        "name": os.path.basename(path),
        "mode": "faketime" if "faketime" in os.path.basename(path) else "baseline",
        "n_samples": len(perceived),
        "interval": interval,
        "threshold": threshold,
        "perceived": perceived,
        "real": real,
        "pauses": pauses,
    }


def pct(values, q):
    if not values:
        return float("nan")
    s = sorted(values)
    idx = min(len(s) - 1, int(round((q / 100.0) * (len(s) - 1))))
    return s[idx]


def report(res):
    p = res["pauses"]
    print(f"\n=== {res['name']}  (mode: {res['mode']}) ===")
    print(f"  samples                 : {res['n_samples']}")
    print(f"  sampling interval (med) : {res['interval'] / 1000:.3f} ms")
    print(f"  pauses detected         : {len(p)}")
    if not p:
        print("  no pauses detected -- was the pause train actually injected?")
        return

    pause_d = [x["pause"] for x in p]
    resid = [x["residual"] for x in p]
    hidden = [x["hidden"] for x in p]
    frac = [100.0 * h / d for h, d in zip(hidden, pause_d) if d > 0]

    print(f"  real pause duration     : median {statistics.median(pause_d)/1000:8.2f} ms"
          f"   (min {min(pause_d)/1000:.2f}, max {max(pause_d)/1000:.2f})")
    print("")
    print("  PERCEIVED TIME GAP (what the app still saw across each pause):")
    print(f"    median                : {statistics.median(resid)/1000:8.3f} ms")
    print(f"    mean                  : {statistics.mean(resid)/1000:8.3f} ms")
    print(f"    p95                   : {pct(resid, 95)/1000:8.3f} ms")
    print(f"    max                   : {max(resid)/1000:8.3f} ms")
    print("")
    print(f"  time hidden by faketime : median {statistics.median(hidden)/1000:8.2f} ms"
          f"  ({statistics.median(frac):.2f}% of the pause)" if frac else "")


def summarize(results):
    print("\n" + "=" * 74)
    print("SUMMARY")
    print("=" * 74)
    hdr = f"{'mode':<10} {'pauses':>7} {'pause (ms)':>12} {'perceived gap (ms)':>21} {'hidden':>8}"
    print(hdr)
    print(f"{'':<10} {'':>7} {'median':>12} {'median':>10}{'p95':>11} {'median':>8}")
    print("-" * 74)
    for r in results:
        p = r["pauses"]
        if not p:
            print(f"{r['mode']:<10} {0:>7}   (no pauses detected)")
            continue
        pause_d = [x["pause"] for x in p]
        resid = [x["residual"] for x in p]
        hidden = [x["hidden"] for x in p]
        frac = [100.0 * h / d for h, d in zip(hidden, pause_d) if d > 0]
        print(
            f"{r['mode']:<10} {len(p):>7} {statistics.median(pause_d)/1000:>12.2f}"
            f" {statistics.median(resid)/1000:>10.3f}{pct(resid,95)/1000:>11.3f}"
            f" {statistics.median(frac):>7.1f}%"
        )
    print("-" * 74)
    print("'perceived gap' is the residual: wall-clock time that still leaked into")
    print("the application's clock across a pause. Lower is better; the baseline row")
    print("(no faketime) shows what the app sees when DDB does not correct for it.")


def plot(results, out_dir, pause_ms):
    try:
        import matplotlib
        matplotlib.use("Agg")
        import matplotlib.pyplot as plt
    except ImportError:
        print("\n(matplotlib not installed -- skipping the figure; "
              "pip install matplotlib to get it)")
        return None

    plt.rcParams.update({
        "font.family": "serif",
        "font.size": 9,
        "axes.edgecolor": INK_MUTED,
        "axes.labelcolor": INK_PRIMARY,
        "text.color": INK_PRIMARY,
        "xtick.color": INK_SECONDARY,
        "ytick.color": INK_SECONDARY,
    })

    fig, (ax1, ax2) = plt.subplots(1, 2, figsize=(9, 3.1))

    # ── Panel 1: how much time DDB has taken off the app's clock ────────────
    # real - perceived, i.e. the accumulated faketime offset as the app actually
    # experiences it. One tread per pause: flat while the app runs, a step up the
    # size of the pause each time DDB stops it. Plotting the two timelines against
    # each other instead would be a pair of near-identical diagonals, with the
    # 100ms steps invisible at a 25s scale -- the difference is the whole story, so
    # plot the difference.
    for res, color in zip(results, SERIES_COLORS):
        xs = [r / 1e6 for r in res["real"]]
        ys = [(r - p) / 1000.0 for r, p in zip(res["real"], res["perceived"])]
        ax1.plot(xs, ys, color=color, linewidth=2, label=res["mode"], zorder=3)
        # Direct-labelled as well as in the legend: aqua sits below 3:1 contrast on
        # a light surface, so identity must not rest on a legend swatch alone.
        ax1.annotate(
            res["mode"], xy=(xs[-1], ys[-1]), xytext=(-3, 5),
            textcoords="offset points", color=color, fontsize=8,
            ha="right", va="bottom", fontweight="bold", zorder=4,
        )

    ax1.set_xlabel("Real time elapsed (s)")
    ax1.set_ylabel("Time hidden from the app (ms)")
    ax1.set_title("DDB takes each pause off the app's clock",
                  fontsize=9.5, pad=8)
    ax1.grid(True, color=INK_MUTED, alpha=0.18, linewidth=0.6)
    ax1.set_axisbelow(True)
    ax1.margins(y=0.12)
    for side in ("top", "right"):
        ax1.spines[side].set_visible(False)
    if len(results) > 1:
        ax1.legend(frameon=False, fontsize=8, loc="upper left")

    # ── Panel 2: the residual, per pause ────────────────────────────────────
    # The headline number. Log scale because the two modes are expected to land
    # orders of magnitude apart, not a few percent.
    for res, color in zip(results, SERIES_COLORS):
        resid = [max(x["residual"], 1) / 1000 for x in res["pauses"]]  # ms, clamped for log
        if not resid:
            continue
        ax2.scatter(range(1, len(resid) + 1), resid, s=26, color=color,
                    edgecolor="white", linewidth=0.8, zorder=3, label=res["mode"])
        med = statistics.median(resid)
        ax2.axhline(med, color=color, linewidth=1, linestyle=(0, (4, 3)),
                    alpha=0.7, zorder=2)
        ax2.annotate(f"{res['mode']}: median {med:.3g} ms",
                     xy=(1, med), xytext=(2, 4), textcoords="offset points",
                     color=color, fontsize=7.5, fontweight="bold", zorder=4)

    ax2.axhline(pause_ms, color=INK_MUTED, linewidth=1, linestyle=":", zorder=1)
    ax2.annotate(f"injected pause ({pause_ms} ms)", xy=(1, pause_ms),
                 xytext=(2, -10), textcoords="offset points",
                 color=INK_MUTED, fontsize=7.5)

    ax2.set_yscale("log")
    # Pauses are counted, not measured -- no "pause 2.5".
    ax2.xaxis.set_major_locator(
        matplotlib.ticker.MaxNLocator(integer=True, nbins="auto")
    )
    ax2.set_xlabel("Pause")
    ax2.set_ylabel("Perceived gap (ms)")
    ax2.set_title("Time the app still perceived across each pause",
                  fontsize=9.5, pad=8)
    ax2.grid(True, color=INK_MUTED, alpha=0.18, linewidth=0.6)
    ax2.set_axisbelow(True)
    for side in ("top", "right"):
        ax2.spines[side].set_visible(False)

    fig.tight_layout()
    png = os.path.join(out_dir, "perceived_gap.png")
    pdf = os.path.join(out_dir, "perceived_gap.pdf")
    fig.savefig(png, dpi=200)
    fig.savefig(pdf)
    return png, pdf


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("csv", nargs="+", help="samples CSV written by faketime_pause")
    ap.add_argument("--pause-ms", type=float, default=100.0,
                    help="the pause duration that was injected (default: 100)")
    ap.add_argument("--out-dir", default=None,
                    help="where to write the figure (default: next to the first CSV)")
    args = ap.parse_args()

    out_dir = args.out_dir or os.path.dirname(os.path.abspath(args.csv[0]))
    os.makedirs(out_dir, exist_ok=True)

    results = [analyze(p, args.pause_ms) for p in args.csv]
    for r in results:
        report(r)
    summarize(results)

    figs = plot(results, out_dir, args.pause_ms)
    if figs:
        print(f"\nFigure: {figs[0]}\n        {figs[1]}")


if __name__ == "__main__":
    main()
