"""Data loading (card notes/cs336-a1/17-data-loading.md)."""

from __future__ import annotations

import numpy as np
import torch


def get_batch(
    x: np.ndarray, batch_size: int, context_length: int, device: str
) -> tuple[torch.Tensor, torch.Tensor]:
    """Sample (inputs, targets) of shape (batch_size, context_length) from a 1-D token array.

    Works with np.memmap: slicing + astype copies only the sampled windows.
    """
    n = len(x)
    if n < context_length + 1:
        raise ValueError("dataset too small for the requested context_length")
    starts = np.random.randint(0, n - context_length, size=batch_size)
    inputs = np.stack([x[i : i + context_length] for i in starts]).astype(np.int64)
    targets = np.stack([x[i + 1 : i + context_length + 1] for i in starts]).astype(np.int64)
    return (
        torch.from_numpy(inputs).to(device),
        torch.from_numpy(targets).to(device),
    )
