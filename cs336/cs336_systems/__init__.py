"""CS336 Assignment 2 (systems): profiling, FlashAttention-2, DDP, sharding, FSDP.

Implemented following the study cards in notes/cs336-a2/.
Distributed components are testable on CPU with the gloo backend; the Triton
kernel requires a CUDA GPU.
"""

from .flash_attention import (
    FlashAttentionPyTorch,
    flash_attention_forward_pytorch,
    flash_attention_backward,
)
from .ddp import (
    broadcast_parameters,
    naive_ddp_all_reduce_grads,
    flat_ddp_all_reduce_grads,
    DDPOverlapIndividualParameters,
)
from .sharded_optimizer import ShardedOptimizer
from .fsdp import FSDP

__all__ = [
    "FlashAttentionPyTorch",
    "flash_attention_forward_pytorch",
    "flash_attention_backward",
    "broadcast_parameters",
    "naive_ddp_all_reduce_grads",
    "flat_ddp_all_reduce_grads",
    "DDPOverlapIndividualParameters",
    "ShardedOptimizer",
    "FSDP",
]
