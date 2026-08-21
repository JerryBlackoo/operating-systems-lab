"""Deduplication (cards notes/cs336-a4/07, 08).

- exact_line_deduplication: two passes; remove every line that occurs more
  than once across the whole corpus (hashed keys to bound memory).
- minhash_deduplication: word-ngram minhash signatures -> LSH banding ->
  true Jaccard verification -> union-find clustering -> keep one per cluster.
"""

from __future__ import annotations

import hashlib
import os
import random
import re
import string
import unicodedata
from collections import Counter, defaultdict
from itertools import combinations
from pathlib import Path

try:
    import mmh3

    def _hash_ngram(ngram: str, seed: int) -> int:
        return mmh3.hash(ngram, seed, signed=False)

except ImportError:  # pragma: no cover - fallback when mmh3 is unavailable

    def _hash_ngram(ngram: str, seed: int) -> int:
        digest = hashlib.blake2b(ngram.encode("utf-8"), digest_size=8, salt=seed.to_bytes(4, "little") + b"\x00" * 12).digest()
        return int.from_bytes(digest, "little")


# --------------------------------------------------------------- exact (card 07)
def _line_key(line: str) -> str:
    return hashlib.md5(line.encode("utf-8")).hexdigest()


def exact_line_deduplication(input_paths: list[str], output_dir: str) -> None:
    """Rewrite each input file into output_dir keeping only corpus-unique lines."""
    os.makedirs(output_dir, exist_ok=True)
    counts: Counter = Counter()
    for path in input_paths:  # pass 1: count line hashes
        with open(path, encoding="utf-8") as f:
            for line in f:
                counts[_line_key(line)] += 1
    for path in input_paths:  # pass 2: rewrite unique lines, preserving order
        out_path = os.path.join(output_dir, Path(path).name)
        with open(path, encoding="utf-8") as fin, open(out_path, "w", encoding="utf-8") as fout:
            for line in fin:
                if counts[_line_key(line)] == 1:
                    fout.write(line)


# ------------------------------------------------------------- minhash (card 08)
_PUNCT_TABLE = str.maketrans("", "", string.punctuation)
_WS_RE = re.compile(r"\s+")


def normalize_text(text: str) -> str:
    """Lowercase, strip accents (NFD), remove punctuation, collapse whitespace."""
    text = text.lower()
    text = unicodedata.normalize("NFD", text)
    text = "".join(c for c in text if not unicodedata.combining(c))
    text = text.translate(_PUNCT_TABLE)
    return _WS_RE.sub(" ", text).strip()


def _word_ngrams(text: str, n: int) -> set[str]:
    words = text.split()
    if len(words) < n:
        return {" ".join(words)} if words else set()
    return {" ".join(words[i : i + n]) for i in range(len(words) - n + 1)}


def _minhash_signature(ngrams: set[str], num_hashes: int) -> list[int]:
    return [min(_hash_ngram(g, seed) for g in ngrams) for seed in range(num_hashes)]


def _jaccard(a: set[str], b: set[str]) -> float:
    if not a and not b:
        return 1.0
    return len(a & b) / len(a | b)


class _UnionFind:
    def __init__(self, n: int):
        self.parent = list(range(n))

    def find(self, x: int) -> int:
        while self.parent[x] != x:
            self.parent[x] = self.parent[self.parent[x]]
            x = self.parent[x]
        return x

    def union(self, a: int, b: int) -> None:
        ra, rb = self.find(a), self.find(b)
        if ra != rb:
            self.parent[rb] = ra


def minhash_deduplication(
    input_paths: list[str],
    num_hashes: int,
    num_bands: int,
    ngram_length: int,
    jaccard_threshold: float,
    output_dir: str,
    seed: int = 0,
) -> None:
    """Fuzzy document dedup: one document per input file; only survivors are
    rewritten into output_dir (one randomly kept per duplicate cluster)."""
    if num_hashes % num_bands != 0:
        raise ValueError("num_hashes must be divisible by num_bands")
    rows_per_band = num_hashes // num_bands
    os.makedirs(output_dir, exist_ok=True)

    docs = []
    ngram_sets = []
    for path in input_paths:
        with open(path, encoding="utf-8") as f:
            text = f.read()
        docs.append(text)
        ngram_sets.append(_word_ngrams(normalize_text(text), ngram_length))

    signatures = [_minhash_signature(ng, num_hashes) if ng else None for ng in ngram_sets]

    # LSH banding: same band values -> same bucket -> candidate pair.
    buckets: dict[tuple, list[int]] = defaultdict(list)
    for doc_idx, sig in enumerate(signatures):
        if sig is None:
            continue
        for band in range(num_bands):
            key = (band, tuple(sig[band * rows_per_band : (band + 1) * rows_per_band]))
            buckets[key].append(doc_idx)

    candidate_pairs = set()
    for members in buckets.values():
        if len(members) > 1:
            candidate_pairs.update(combinations(sorted(members), 2))

    # Verify candidates with the true ngram Jaccard, then cluster transitively.
    uf = _UnionFind(len(docs))
    for i, j in candidate_pairs:
        if _jaccard(ngram_sets[i], ngram_sets[j]) >= jaccard_threshold:
            uf.union(i, j)

    clusters: dict[int, list[int]] = defaultdict(list)
    for idx in range(len(docs)):
        clusters[uf.find(idx)].append(idx)

    rng = random.Random(seed)
    survivors = set()
    for members in clusters.values():
        survivors.add(rng.choice(members))

    for idx, path in enumerate(input_paths):
        if idx in survivors:
            out_path = os.path.join(output_dir, Path(path).name)
            with open(out_path, "w", encoding="utf-8") as f:
                f.write(docs[idx])
