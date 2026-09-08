"""CS336 Assignment 1 (basics): BPE tokenizer + Transformer LM, from scratch.

Implemented following the study cards in notes/cs336-a1/.
"""

from .bpe import train_bpe, save_vocab_and_merges, load_vocab_and_merges
from .tokenizer import Tokenizer
from .model import (
    Linear,
    Embedding,
    RMSNorm,
    SwiGLU,
    RotaryPositionalEmbedding,
    softmax,
    scaled_dot_product_attention,
    MultiHeadSelfAttention,
    TransformerBlock,
    TransformerLM,
)
from .optim import AdamW, cross_entropy, perplexity, lr_cosine_schedule, gradient_clipping
from .data import get_batch
from .checkpoint import save_checkpoint, load_checkpoint
from .decode import generate, top_p_filter

__all__ = [
    "train_bpe",
    "save_vocab_and_merges",
    "load_vocab_and_merges",
    "Tokenizer",
    "Linear",
    "Embedding",
    "RMSNorm",
    "SwiGLU",
    "RotaryPositionalEmbedding",
    "softmax",
    "scaled_dot_product_attention",
    "MultiHeadSelfAttention",
    "TransformerBlock",
    "TransformerLM",
    "AdamW",
    "cross_entropy",
    "perplexity",
    "lr_cosine_schedule",
    "gradient_clipping",
    "get_batch",
    "save_checkpoint",
    "load_checkpoint",
    "generate",
    "top_p_filter",
]
