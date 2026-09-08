"""Gopher quality rules (card notes/cs336-a4/05-gopher-quality-filters.md).

Uses nltk.word_tokenize when available, otherwise a simple regex fallback
(the handout says NLTK is optional).
"""

from __future__ import annotations

import re

try:  # pragma: no cover - depends on environment
    import nltk

    def _word_tokenize(text: str) -> list[str]:
        try:
            return nltk.word_tokenize(text)
        except LookupError:  # punkt not downloaded
            return _regex_tokenize(text)

except ImportError:  # pragma: no cover

    def _word_tokenize(text: str) -> list[str]:
        return _regex_tokenize(text)


_WORD_RE = re.compile(r"[A-Za-z0-9']+|[^\sA-Za-z0-9]")


def _regex_tokenize(text: str) -> list[str]:
    return _WORD_RE.findall(text)


def gopher_quality_filter(text: str) -> bool:
    """True iff the document passes ALL of the following rules:
    1. 50 <= word count <= 100,000
    2. 3 <= mean word length <= 10
    3. <= 30% of lines end with an ellipsis ("...")
    4. >= 80% of words contain at least one alphabetic character
    """
    words = _word_tokenize(text)
    if not words:
        return False

    n_words = len(words)
    if n_words < 50 or n_words > 100_000:
        return False

    mean_len = sum(len(w) for w in words) / n_words
    if mean_len < 3 or mean_len > 10:
        return False

    lines = text.split("\n")
    if lines:
        ellipsis_lines = sum(
            1 for line in lines if line.rstrip().endswith(("...", "\u2026"))
        )
        if ellipsis_lines / len(lines) > 0.30:
            return False

    alpha_words = sum(1 for w in words if any(c.isalpha() for c in w))
    if alpha_words / n_words < 0.80:
        return False

    return True
