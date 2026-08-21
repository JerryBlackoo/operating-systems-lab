"""BPE tokenizer encode/decode (card notes/cs336-a1/02-tokenizer.md)."""

from __future__ import annotations

from collections.abc import Iterable, Iterator

import regex as re

from .bpe import PAT, load_vocab_and_merges

_PRETOKEN_RE = re.compile(PAT)
_INF = float("inf")


class Tokenizer:
    def __init__(
        self,
        vocab: dict[int, bytes],
        merges: list[tuple[bytes, bytes]],
        special_tokens: list[str] | None = None,
    ):
        self.vocab: dict[int, bytes] = dict(vocab)
        self.special_tokens = list(special_tokens or [])

        # Append missing special tokens to the end of the vocab.
        existing = set(self.vocab.values())
        next_id = max(self.vocab) + 1 if self.vocab else 0
        for st in self.special_tokens:
            b = st.encode("utf-8")
            if b not in existing:
                self.vocab[next_id] = b
                existing.add(b)
                next_id += 1

        self.bytes_to_id = {v: k for k, v in self.vocab.items()}
        self.merge_ranks = {pair: rank for rank, pair in enumerate(merges)}

        if self.special_tokens:
            # Longest-first alternation with a capture group so split keeps the specials.
            alternation = "|".join(
                re.escape(t) for t in sorted(self.special_tokens, key=len, reverse=True)
            )
            self._special_re = re.compile(f"({alternation})")
            self._special_set = set(self.special_tokens)
        else:
            self._special_re = None
            self._special_set = set()

        self._cache: dict[str, list[int]] = {}

    @classmethod
    def from_files(cls, vocab_filepath, merges_filepath, special_tokens=None) -> "Tokenizer":
        vocab, merges = load_vocab_and_merges(vocab_filepath, merges_filepath)
        return cls(vocab, merges, special_tokens)

    def _bpe(self, pretoken: str) -> list[int]:
        cached = self._cache.get(pretoken)
        if cached is not None:
            return cached
        word = [bytes([b]) for b in pretoken.encode("utf-8")]
        while len(word) >= 2:
            # Earliest-created applicable merge.
            best_pair = min(
                zip(word, word[1:]),
                key=lambda p: self.merge_ranks.get(p, _INF),
            )
            if best_pair not in self.merge_ranks:
                break
            a, b = best_pair
            merged = a + b
            new_word: list[bytes] = []
            i = 0
            while i < len(word):
                if i < len(word) - 1 and word[i] == a and word[i + 1] == b:
                    new_word.append(merged)
                    i += 2
                else:
                    new_word.append(word[i])
                    i += 1
            word = new_word
        ids = [self.bytes_to_id[t] for t in word]
        self._cache[pretoken] = ids
        return ids

    def encode(self, text: str) -> list[int]:
        ids: list[int] = []
        parts = self._special_re.split(text) if self._special_re else [text]
        for part in parts:
            if not part:
                continue
            if part in self._special_set:
                ids.append(self.bytes_to_id[part.encode("utf-8")])
                continue
            for m in _PRETOKEN_RE.finditer(part):
                ids.extend(self._bpe(m.group()))
        return ids

    def encode_iterable(self, iterable: Iterable[str]) -> Iterator[int]:
        """Lazily encode chunks (e.g., lines of a file handle). O(chunk) memory."""
        for chunk in iterable:
            yield from self.encode(chunk)

    def decode(self, ids: list[int]) -> str:
        # Concatenate bytes first, decode once: multi-byte UTF-8 characters can span ids.
        data = b"".join(self.vocab[i] for i in ids)
        return data.decode("utf-8", errors="replace")
