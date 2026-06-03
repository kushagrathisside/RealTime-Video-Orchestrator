#!/usr/bin/env python3
"""Generate paper figures from RVO bench CSVs.

Usage:
    python3 scripts/plot.py --in-dir target/bench_results --out-dir docs/report/figures

Produces:
    fig1_tick_p99_vs_detector_latency.pdf   — HOL blocking effect
    fig2_load_shedding.pdf                  — load-shedding in action (time-series)
    fig3_throughput_vs_fps.pdf              — graceful degradation under overload
    fig4_tick_cdf.pdf                       — tick duration CDF per scenario
"""
import argparse
import pathlib
import sys

try:
    import pandas as pd
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    import matplotlib.ticker as ticker
    import numpy as np
except ImportError:
    print("Install: pip install pandas matplotlib numpy")
    sys.exit(1)

LABEL_MAP = {
    "baseline":     "No detectors",
    "inproc_low":   "DummyDetector (~0ms)",
    "blocking_1ms": "Blocking 1ms",
    "blocking_3ms": "Blocking 3ms",
    "blocking_10ms":"Blocking 10ms",
    "blocking_50ms":"Blocking 50ms",
    "load_shed":    "Dummy + Blocking 50ms",
    "fps_30":   "30 fps",
    "fps_60":   "60 fps",
    "fps_120":  "120 fps",
    "fps_300":  "300 fps",
}

PLT_STYLE = {
    "figure.figsize": (7, 4),
    "axes.spines.top": False,
    "axes.spines.right": False,
    "font.size": 11,
}


def ns_to_ms(v):
    return v / 1e6


def load_summary(in_dir: pathlib.Path) -> pd.DataFrame:
    p = in_dir / "summary.csv"
    if not p.exists():
        print(f"[plot] summary.csv not found in {in_dir}"); sys.exit(1)
    df = pd.read_csv(p)
    df["tick_p99_ms"]  = ns_to_ms(df["tick_p99_ns"])
    df["tick_p999_ms"] = ns_to_ms(df["tick_p999_ns"])
    df["tick_p50_ms"]  = ns_to_ms(df["tick_p50_ns"])
    return df


def load_timeseries(in_dir: pathlib.Path, scenario: str) -> pd.DataFrame | None:
    candidates = list(in_dir.glob(f"{scenario}_*_timeseries.csv"))
    if not candidates:
        return None
    df = pd.read_csv(candidates[-1])
    df["elapsed_s"] = df["elapsed_ms"] / 1000
    df["tick_p99_ms"] = ns_to_ms(df["tick_p99_ns"])
    return df


# ── Figure 1: tick p99 vs configured detector latency ───────────────────────

def fig_tick_p99_vs_latency(df: pd.DataFrame, out: pathlib.Path):
    blocking = df[df["scenario"].str.startswith("blocking_")].copy()
    baseline = df[df["scenario"] == "baseline"]

    if blocking.empty:
        print("[plot] No blocking scenarios in summary; skipping fig1"); return

    with plt.rc_context(PLT_STYLE):
        fig, ax = plt.subplots()
        ax.plot(blocking["detector_sleep_ms"], blocking["tick_p99_ms"],
                marker="o", linewidth=2, label="RVO tick p99")
        ax.plot(blocking["detector_sleep_ms"], blocking["tick_p999_ms"],
                marker="s", linestyle="--", linewidth=1.5, label="RVO tick p99.9")
        if not baseline.empty:
            ax.axhline(baseline["tick_p99_ms"].iloc[0], color="grey",
                       linestyle=":", linewidth=1.5, label="Baseline (no detectors)")
        ax.set_xlabel("Detector sleep (ms)")
        ax.set_ylabel("Tick latency (ms)")
        ax.set_title("HOL blocking: tick p99 vs in-process detector latency")
        ax.legend()
        ax.grid(axis="y", alpha=0.3)
        fig.tight_layout()
        p = out / "fig1_tick_p99_vs_detector_latency.pdf"
        fig.savefig(p); print(f"[plot] wrote {p}")
        plt.close(fig)


# ── Figure 2: load-shedding time-series ─────────────────────────────────────

def fig_load_shedding(in_dir: pathlib.Path, out: pathlib.Path):
    df = load_timeseries(in_dir, "load_shed")
    if df is None:
        print("[plot] load_shed timeseries not found; skipping fig2"); return

    with plt.rc_context(PLT_STYLE):
        fig, (ax1, ax2) = plt.subplots(2, 1, sharex=True, figsize=(7, 6))

        ax1.plot(df["elapsed_s"], df["tick_p99_ms"], linewidth=1.5, color="steelblue")
        ax1.set_ylabel("Tick p99 (ms)")
        ax1.set_title("Load-shedding: slow detector + DummyDetector running together")
        ax1.grid(axis="y", alpha=0.3)

        ax2.bar(df["elapsed_s"], df["skips_delta"], width=0.9,
                color="orange", alpha=0.85, label="skips/interval")
        ax2.bar(df["elapsed_s"], df["frame_drops_delta"], width=0.9,
                color="crimson", alpha=0.7, label="frame_drops/interval", bottom=0)
        ax2.set_xlabel("Elapsed (s)")
        ax2.set_ylabel("Count per interval")
        ax2.legend(loc="upper right")
        ax2.grid(axis="y", alpha=0.3)

        fig.tight_layout()
        p = out / "fig2_load_shedding.pdf"
        fig.savefig(p); print(f"[plot] wrote {p}")
        plt.close(fig)


# ── Figure 3: throughput vs fps ──────────────────────────────────────────────

def fig_throughput_vs_fps(df: pd.DataFrame, out: pathlib.Path):
    fps_rows = df[df["scenario"].str.startswith("fps_")].copy()
    if fps_rows.empty:
        print("[plot] No fps scenarios in summary; skipping fig3"); return

    fps_rows = fps_rows.sort_values("input_fps")
    with plt.rc_context(PLT_STYLE):
        fig, ax = plt.subplots()
        ax.plot(fps_rows["input_fps"], fps_rows["total_frame_drops"],
                marker="o", linewidth=2, color="crimson", label="Frame drops (total)")
        ax2 = ax.twinx()
        ax2.plot(fps_rows["input_fps"], fps_rows["total_events"],
                 marker="s", linestyle="--", linewidth=1.5, color="steelblue",
                 label="Events emitted (total)")
        ax.set_xlabel("Camera input fps")
        ax.set_ylabel("Total frame drops", color="crimson")
        ax2.set_ylabel("Total events emitted", color="steelblue")
        ax.set_title("Throughput vs input fps — graceful degradation under overload")
        lines1, labels1 = ax.get_legend_handles_labels()
        lines2, labels2 = ax2.get_legend_handles_labels()
        ax.legend(lines1 + lines2, labels1 + labels2, loc="upper left")
        ax.grid(axis="y", alpha=0.3)
        fig.tight_layout()
        p = out / "fig3_throughput_vs_fps.pdf"
        fig.savefig(p); print(f"[plot] wrote {p}")
        plt.close(fig)


# ── Figure 4: tick CDF per scenario ─────────────────────────────────────────

def fig_tick_cdf(df: pd.DataFrame, out: pathlib.Path):
    blocking_scenarios = ["baseline", "inproc_low", "blocking_3ms", "blocking_10ms"]
    rows = df[df["scenario"].isin(blocking_scenarios)]
    if rows.empty:
        print("[plot] insufficient scenarios for CDF; skipping fig4"); return

    with plt.rc_context(PLT_STYLE):
        fig, ax = plt.subplots()
        quantiles = np.array([0.5, 0.9, 0.95, 0.99, 0.999])
        for _, row in rows.iterrows():
            label = LABEL_MAP.get(row["scenario"], row["scenario"])
            # Approximate CDF from the three reported percentiles.
            p_vals = np.array([row["tick_p50_ms"], row["tick_p99_ms"], row["tick_p999_ms"]])
            interp_q = np.array([0.5, 0.99, 0.999])
            ms_vals = np.interp(quantiles, interp_q, p_vals)
            ax.plot(ms_vals, quantiles * 100, marker=".", label=label)

        ax.set_xlabel("Tick latency (ms)")
        ax.set_ylabel("Percentile (%)")
        ax.set_title("Tick duration CDF — in-process detector latency effect")
        ax.set_ylim(50, 100)
        ax.yaxis.set_major_formatter(ticker.FormatStrFormatter("%.1f"))
        ax.legend(fontsize=9)
        ax.grid(alpha=0.3)
        fig.tight_layout()
        p = out / "fig4_tick_cdf.pdf"
        fig.savefig(p); print(f"[plot] wrote {p}")
        plt.close(fig)


# ── main ─────────────────────────────────────────────────────────────────────

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--in-dir",  default="target/bench_results",   type=pathlib.Path)
    ap.add_argument("--out-dir", default="docs/report/figures",     type=pathlib.Path)
    args = ap.parse_args()

    args.out_dir.mkdir(parents=True, exist_ok=True)
    df = load_summary(args.in_dir)

    fig_tick_p99_vs_latency(df, args.out_dir)
    fig_load_shedding(args.in_dir, args.out_dir)
    fig_throughput_vs_fps(df, args.out_dir)
    fig_tick_cdf(df, args.out_dir)

    print(f"\n[plot] figures written to {args.out_dir}/")


if __name__ == "__main__":
    main()
