"""Training API client for the scaling-laws leaderboard (card notes/cs336-a3/02).

Every response is cached to disk so no budget is ever spent twice on lost
results. Requires the Stanford network for real calls; --dry-run prints the
request without sending it.

    python -m cs336_scaling.api_client --api-key 06123456 budget
    python -m cs336_scaling.api_client --api-key 06123456 submit --config cfg.json --dry-run
"""

from __future__ import annotations

import argparse
import json
import os
import time

API_BASE_URL = os.environ.get("CS336_SCALING_API", "http://hyperturing.stanford.edu:8000")


def default_config(
    hidden_size: int = 448,
    num_hidden_layers: int = 9,
    num_attention_heads: int = 7,
    head_dim: int = 64,
    intermediate_size: int = 1280,
    lr: float = 3e-4,
    train_batch_size: int = 128,
    total_train_tokens: int = 1_048_576,
    max_runtime_seconds: float = 30.0,
    model_seed: int = 0,
) -> dict:
    """A valid /submit body (constraints: hidden = heads*head_dim,
    total_train_tokens % (512*batch) == 0)."""
    assert hidden_size == num_attention_heads * head_dim
    assert total_train_tokens % (512 * train_batch_size) == 0
    return {
        "architecture_config": {
            "attention_bias": False,
            "head_dim": head_dim,
            "hidden_size": hidden_size,
            "intermediate_size": intermediate_size,
            "num_attention_heads": num_attention_heads,
            "num_hidden_layers": num_hidden_layers,
            "num_key_value_heads": num_attention_heads,
            "rms_norm_eps": 1e-6,
            "rope_theta": 1_000_000,
            "tie_word_embeddings": False,
            "dtype": "bfloat16",
            "vocab_size": 32_000,
        },
        "optimizer_config": {
            "lr_scheduler": {
                "peak_value": lr,
                "final_lr_frac": 0.1,
                "warmup_frac": 0.05,
                "init_value": 0.0,
            },
            "weight_decay": 1e-2,
            "beta1": 0.9,
            "beta2": 0.95,
            "eps": 1e-8,
            "eps_root": 1e-8,
            "grad_clip_norm": 1.0,
        },
        "train_batch_size": train_batch_size,
        "val_batch_size": 32,
        "n_evals": 16,
        "total_train_tokens": total_train_tokens,
        "max_runtime_seconds": max_runtime_seconds,
        "model_seed": model_seed,
    }


def estimate_non_embedding_params(num_layers: int, d_model: int) -> float:
    """Handout: N ~= 12 * n_layer * d_model^2."""
    return 12.0 * num_layers * d_model**2


class ScalingAPI:
    def __init__(self, api_key: str, cache_path: str = "scaling_api_cache.json"):
        self.api_key = api_key
        self.cache_path = cache_path
        self._cache = {}
        if os.path.exists(cache_path):
            with open(cache_path) as f:
                self._cache = json.load(f)

    def _save_cache(self):
        with open(self.cache_path, "w") as f:
            json.dump(self._cache, f, indent=2)

    def _request(self, method: str, path: str, body: dict | None = None):
        import requests

        headers = {"X-API-Key": self.api_key}
        url = f"{API_BASE_URL}{path}"
        resp = requests.request(method, url, headers=headers, json=body, timeout=60)
        resp.raise_for_status()
        return resp.json()

    def budget(self) -> dict:
        return self._request("GET", "/budget")

    def submit(self, training_config: dict) -> dict:
        result = self._request("POST", "/submit", training_config)
        self._cache[f"submit_{result['experiment_id']}"] = {
            "config": training_config, "response": result, "time": time.time(),
        }
        self._save_cache()
        return result

    def experiments(self) -> list[dict]:
        result = self._request("GET", "/experiments")
        self._cache["experiments"] = result
        self._save_cache()
        return result

    def poll_until_complete(self, experiment_id: int, interval_s: float = 30.0) -> dict:
        while True:
            exp = self._request("GET", f"/experiment/{experiment_id}")
            status = exp["status"]["status_type"]
            if status in ("completed", "failed"):
                self._cache[f"experiment_{experiment_id}"] = exp
                self._save_cache()
                return exp
            time.sleep(interval_s)

    def final_submission(self, training_config: dict, predicted_final_loss: float) -> dict:
        body = {"training_config": training_config, "predicted_final_loss": predicted_final_loss}
        return self._request("POST", "/final_submission", body)


def main(argv=None):  # pragma: no cover - needs Stanford network
    p = argparse.ArgumentParser()
    p.add_argument("--api-key", required=True)
    p.add_argument("--dry-run", action="store_true")
    sub = p.add_subparsers(dest="command", required=True)
    sub.add_parser("budget")
    sp = sub.add_parser("submit")
    sp.add_parser = sp.add_argument("--config", help="JSON config file (default: example config)")
    sub.add_parser("experiments")
    args = p.parse_args(argv)

    api = ScalingAPI(args.api_key)
    if args.command == "budget":
        print(json.dumps(api.budget(), indent=2))
    elif args.command == "experiments":
        print(json.dumps(api.experiments(), indent=2))
    elif args.command == "submit":
        config = default_config()
        if getattr(args, "config", None):
            with open(args.config) as f:
                config = json.load(f)
        if args.dry_run:
            print("DRY RUN, would POST /submit:")
            print(json.dumps(config, indent=2))
        else:
            print(json.dumps(api.submit(config), indent=2))


if __name__ == "__main__":
    main()
