"""A3 cards: IsoFLOPs fitting (recovers known exponents; runs on official data)."""

import json
from pathlib import Path

import numpy as np
import pytest

from cs336_scaling.api_client import default_config, estimate_non_embedding_params
from cs336_scaling.isoflops import (
    fit_isoflops_scaling_laws,
    fit_power_law,
    predict_power_law,
)

DATA_PATH = Path(__file__).resolve().parent.parent / "cs336_scaling/data/isoflops_curves.json"


def test_fit_power_law_exact():
    x = np.array([1e18, 1e19, 1e20, 1e21])
    y = 3.0 * x**0.5
    a, k = fit_power_law(x, y)
    assert a == pytest.approx(0.5, abs=1e-9)
    assert k == pytest.approx(3.0, rel=1e-6)


def _synthetic_runs(a_true=0.5, k_true=0.8, noise=0.0, seed=0):
    """IsoFLOPs profiles with a quadratic-in-log(N) loss whose minimum follows
    N_opt = k_true * C^a_true."""
    rng = np.random.default_rng(seed)
    runs = []
    for budget in [1e18, 3e18, 1e19, 3e19, 1e20]:
        n_opt = k_true * budget**a_true
        for factor in [0.25, 0.5, 1.0, 2.0, 4.0]:
            n = n_opt * factor
            loss = 2.0 + 0.5 * np.log(n / n_opt) ** 2 + rng.normal(0, noise)
            runs.append(
                {"parameters": n, "compute_budget": budget, "final_loss": float(loss)}
            )
    return runs


def test_isoflops_recovers_known_exponents():
    fit = fit_isoflops_scaling_laws(_synthetic_runs())
    assert fit["a"] == pytest.approx(0.5, abs=0.02)
    assert fit["b"] == pytest.approx(0.5, abs=0.02)  # b = 1 - a since D = C/(6N)
    assert fit["a"] + fit["b"] == pytest.approx(1.0, abs=1e-9)
    n_pred = predict_power_law(fit["a"], fit["k_n"], 1e23)
    assert n_pred == pytest.approx(0.8 * 1e23**0.5, rel=0.05)


@pytest.mark.skipif(not DATA_PATH.exists(), reason="official isoflops data not present")
def test_isoflops_on_official_data():
    with open(DATA_PATH) as f:
        runs = json.load(f)
    fit = fit_isoflops_scaling_laws(runs)
    # Chinchilla-style data: exponents around 0.5 and complementary.
    assert 0.3 < fit["a"] < 0.7
    assert fit["a"] + fit["b"] == pytest.approx(1.0, abs=1e-6)
    # Extrapolations must be monotone in C and finite.
    n23 = predict_power_law(fit["a"], fit["k_n"], 1e23)
    n24 = predict_power_law(fit["a"], fit["k_n"], 1e24)
    assert np.isfinite(n23) and n24 > n23 > max(fit["n_opts"])


def test_default_api_config_satisfies_constraints():
    cfg = default_config()
    arch = cfg["architecture_config"]
    assert arch["hidden_size"] == arch["num_attention_heads"] * arch["head_dim"]
    assert arch["num_attention_heads"] % arch["num_key_value_heads"] == 0
    assert cfg["total_train_tokens"] % (512 * cfg["train_batch_size"]) == 0
    assert 1 <= cfg["max_runtime_seconds"] <= 12 * 3600


def test_non_embedding_param_estimate():
    # 12 * n_layer * d_model^2 (handout formula)
    assert estimate_non_embedding_params(48, 1600) == 12 * 48 * 1600**2
