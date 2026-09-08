"""Chinchilla IsoFLOPs scaling-law fitting (card notes/cs336-a3/01).

    python -m cs336_scaling.isoflops --data cs336_scaling/data/isoflops_curves.json \
        --plot-dir /tmp/isoflops_plots

C = 6 N D; per budget take the run with the lowest loss as N_opt(C); fit
power laws N_opt ~ k C^a and D_opt ~ k' C^b in log-log space; extrapolate.
"""

from __future__ import annotations

import argparse
import json
from collections import defaultdict

import numpy as np


def fit_power_law(x: np.ndarray, y: np.ndarray) -> tuple[float, float]:
    """Fit y = k * x^a via linear regression in log-log space. Returns (a, k)."""
    slope, intercept = np.polyfit(np.log(x), np.log(y), 1)
    return float(slope), float(np.exp(intercept))


def predict_power_law(a: float, k: float, x: float) -> float:
    return k * x**a


def fit_isoflops_scaling_laws(runs: list[dict]) -> dict:
    """runs: [{"parameters": N, "compute_budget": C, "final_loss": L}, ...]

    Returns exponents/coefficients plus the (C_i, N_opt, D_opt) points used.
    """
    by_budget: dict[float, list[dict]] = defaultdict(list)
    for run in runs:
        by_budget[float(run["compute_budget"])].append(run)

    budgets, n_opts, d_opts, best_losses = [], [], [], []
    for budget in sorted(by_budget):
        best = min(by_budget[budget], key=lambda r: r["final_loss"])
        budgets.append(budget)
        n_opts.append(float(best["parameters"]))
        d_opts.append(budget / (6.0 * best["parameters"]))  # D = C / (6N)
        best_losses.append(float(best["final_loss"]))

    budgets_arr = np.array(budgets)
    a, k_n = fit_power_law(budgets_arr, np.array(n_opts))
    b, k_d = fit_power_law(budgets_arr, np.array(d_opts))
    return {
        "a": a, "k_n": k_n,       # N_opt = k_n * C^a
        "b": b, "k_d": k_d,       # D_opt = k_d * C^b
        "budgets": budgets, "n_opts": n_opts, "d_opts": d_opts,
        "best_losses": best_losses,
    }


def make_plots(fit: dict, plot_dir: str, extrapolate_to: float = 1e24) -> None:
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    import os

    os.makedirs(plot_dir, exist_ok=True)
    grid = np.logspace(np.log10(min(fit["budgets"])), np.log10(extrapolate_to), 100)
    for name, key_pts, exp, coeff in (
        ("model_size", "n_opts", fit["a"], fit["k_n"]),
        ("dataset_size", "d_opts", fit["b"], fit["k_d"]),
    ):
        plt.figure(figsize=(6, 4))
        plt.loglog(fit["budgets"], fit[key_pts], "o", label="IsoFLOPs optima")
        plt.loglog(grid, coeff * grid**exp, "-", label=f"fit: exponent {exp:.3f}")
        plt.xlabel("compute budget C (FLOPs)")
        plt.ylabel("optimal " + name.replace("_", " "))
        plt.legend()
        plt.tight_layout()
        path = os.path.join(plot_dir, f"isoflops_{name}.png")
        plt.savefig(path, dpi=120)
        plt.close()
        print(f"saved {path}")


def main(argv=None):
    p = argparse.ArgumentParser()
    p.add_argument("--data", default="cs336_scaling/data/isoflops_curves.json")
    p.add_argument("--plot-dir", default=None)
    args = p.parse_args(argv)

    with open(args.data) as f:
        runs = json.load(f)
    fit = fit_isoflops_scaling_laws(runs)
    print(f"N_opt = {fit['k_n']:.3e} * C^{fit['a']:.4f}")
    print(f"D_opt = {fit['k_d']:.3e} * C^{fit['b']:.4f}")
    print(f"sanity: a + b = {fit['a'] + fit['b']:.4f} (should be ~1 since N*D ~ C/6)")
    for target in (1e23, 1e24):
        n = predict_power_law(fit["a"], fit["k_n"], target)
        d = predict_power_law(fit["b"], fit["k_d"], target)
        print(f"C = {target:.0e}: N_opt ~ {n:.3e} params, D_opt ~ {d:.3e} tokens "
              f"(6ND = {6 * n * d:.2e})")
    if args.plot_dir:
        make_plots(fit, args.plot_dir)
    return fit


if __name__ == "__main__":
    main()
