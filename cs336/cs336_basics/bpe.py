"""BPE tokenizer training (card notes/cs336-a1/01-train-bpe.md).

train_bpe(input_path, vocab_size, special_tokens) -> (vocab, merges)
  vocab: dict[int, bytes]
  merges: list[tuple[bytes, bytes]] in creation order
"""

from __future__ import annotations

import os
import pickle
from collections import Counter, defaultdict
from multiprocessing import Pool

import regex as re

# GPT-2 pre-tokenization regex (requires the `regex` package, not builtin `re`).
PAT = r"""'(?:[sdmt]|ll|ve|re)| ?\p{L}+| ?\p{N}+| ?[^\s\p{L}\p{N}]+|\s+(?!\S)|\s+"""
_PRETOKEN_RE = re.compile(PAT)


def split_on_special_tokens(text: str, special_tokens: list[str]) -> list[str]:
    """Split text on special tokens (hard merge boundaries). Specials are dropped."""
    if not special_tokens:
        return [text]
    # Longest first so overlapping specials match greedily.
    pattern = "|".join(re.escape(t) for t in sorted(special_tokens, key=len, reverse=True))
    return re.split(pattern, text)


def _count_pretokens(parts: list[str]) -> Counter:
    """Count pre-token frequencies; each pre-token is a tuple of single-byte bytes."""
    counts: Counter = Counter()
    for part in parts:
        for m in _PRETOKEN_RE.finditer(part):
            b = m.group().encode("utf-8")
            counts[tuple(bytes([c]) for c in b)] += 1
    return counts


def _merge_word(word: list[bytes], a: bytes, b: bytes, merged: bytes) -> list[bytes]:
    out: list[bytes] = []
    i = 0
    n = len(word)
    while i < n:
        if i < n - 1 and word[i] == a and word[i + 1] == b:
            out.append(merged)
            i += 2
        else:
            out.append(word[i])
            i += 1
    return out


def train_bpe(
    input_path: str | os.PathLike,
    vocab_size: int,
    special_tokens: list[str],
    num_workers: int = 1,
) -> tuple[dict[int, bytes], list[tuple[bytes, bytes]]]:
    """Train a byte-level BPE tokenizer.

    Vocabulary layout: special tokens, then the 256 byte values, then merged tokens
    in creation order (matches the handout's bpe_example).
    """
    special_tokens = list(special_tokens or [])
    num_merges = vocab_size - 256 - len(special_tokens)
    if num_merges < 0:
        raise ValueError("vocab_size must be >= 256 + len(special_tokens)")

    with open(input_path, encoding="utf-8") as f:
        text = f.read()

    # 1) Split on special tokens (they never participate in merges), 2) pre-tokenize.
    parts = split_on_special_tokens(text, special_tokens)
    if num_workers > 1 and len(parts) > 1:
        chunk_size = max(1, len(parts) // num_workers)
        chunks = [parts[i : i + chunk_size] for i in range(0, len(parts), chunk_size)]
        with Pool(num_workers) as pool:
            partial_counts = pool.map(_count_pretokens, chunks)
        word_freqs: Counter = Counter()
        for c in partial_counts:
            word_freqs.update(c)
    else:
        word_freqs = _count_pretokens(parts)

    # Mutable word list + pair index for incremental updates.
    words: list[list[bytes]] = []
    freqs: list[int] = []
    pair_counts: Counter = Counter()
    pair_to_words: dict[tuple[bytes, bytes], set[int]] = defaultdict(set)

    for word, freq in word_freqs.items():
        idx = len(words)
        words.append(list(word))
        freqs.append(freq)
        for i in range(len(word) - 1):
            pair = (word[i], word[i + 1])
            pair_counts[pair] += freq
            pair_to_words[pair].add(idx)

    merges: list[tuple[bytes, bytes]] = []
    for _ in range(num_merges):
        if not pair_counts:
            break
        # Highest count; ties broken by lexicographically greater pair (max()).
        best_pair = max(pair_counts.items(), key=lambda kv: (kv[1], kv[0]))[0]
        a, b = best_pair
        merged = a + b
        merges.append(best_pair)

        affected = list(pair_to_words.get(best_pair, ()))
        for widx in affected:
            word = words[widx]
            freq = freqs[widx]
            # Remove old pair counts for this word.
            for i in range(len(word) - 1):
                pair = (word[i], word[i + 1])
                pair_counts[pair] -= freq
                if pair_counts[pair] <= 0:
                    del pair_counts[pair]
                pair_to_words[pair].discard(widx)
            # Apply the merge and add new pair counts.
            new_word = _merge_word(word, a, b, merged)
            words[widx] = new_word
            for i in range(len(new_word) - 1):
                pair = (new_word[i], new_word[i + 1])
                pair_counts[pair] += freq
                pair_to_words[pair].add(widx)

    vocab: dict[int, bytes] = {}
    idx = 0
    for st in special_tokens:
        vocab[idx] = st.encode("utf-8")
        idx += 1
    for byte_val in range(256):
        vocab[idx] = bytes([byte_val])
        idx += 1
    for pair in merges:
        vocab[idx] = pair[0] + pair[1]
        idx += 1
    return vocab, merges


def save_vocab_and_merges(vocab, merges, vocab_path, merges_path) -> None:
    with open(vocab_path, "wb") as f:
        pickle.dump(vocab, f)
    with open(merges_path, "wb") as f:
        pickle.dump(merges, f)


def load_vocab_and_merges(vocab_path, merges_path):
    with open(vocab_path, "rb") as f:
        vocab = pickle.load(f)
    with open(merges_path, "rb") as f:
        merges = pickle.load(f)
    return vocab, merges
