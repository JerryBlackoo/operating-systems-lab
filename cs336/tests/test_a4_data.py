"""A4 cards: PII masking, Gopher rules, dedup, filter chain (pure-python parts)."""

import pytest

from cs336_data.dedup import (
    _jaccard,
    _word_ngrams,
    exact_line_deduplication,
    minhash_deduplication,
    normalize_text,
)
from cs336_data.gopher import gopher_quality_filter
from cs336_data.pii import mask_emails, mask_ips, mask_phone_numbers
from cs336_data.pipeline import filter_document
from collections import Counter


# ------------------------------------------------------------------------ PII
def test_mask_emails():
    text = "contact john.doe+spam@example.co.uk or admin@test.org today"
    out, n = mask_emails(text)
    assert n == 2
    assert "|||EMAIL_ADDRESS|||" in out
    assert "@" not in out


def test_mask_phone_numbers_common_us_formats():
    text = "call 650-555-1234 or (650) 555 1234 or 650.555.1234 or 6505551234 or +1 650 555 1234"
    out, n = mask_phone_numbers(text)
    assert n == 5
    assert out.count("|||PHONE_NUMBER|||") == 5


def test_mask_phone_does_not_eat_long_numbers():
    out, n = mask_phone_numbers("order id 123456789012345 is unrelated")
    assert n == 0


def test_mask_ips():
    out, n = mask_ips("server at 192.168.0.1 and 8.8.8.8, but not 999.999.999.999")
    assert n == 2
    assert out.count("|||IP_ADDRESS|||") == 2
    assert "999.999.999.999" in out  # octets > 255 are not IPs


def test_mask_ips_octet_boundaries():
    _, n = mask_ips("255.255.255.255 and 0.0.0.0 and 256.1.1.1")
    assert n == 2


# --------------------------------------------------------------------- Gopher
GOOD_TEXT = (
    "The quick brown fox jumps over the lazy dog near the quiet river bank. "
    "It was a bright morning and the animals gathered to watch the spectacle unfold. "
    "Many of them wondered why the fox was always in such a hurry every single day. "
    "Nevertheless the dog remained calm and patient, content to rest in the warm sun."
)


def test_gopher_accepts_normal_prose():
    assert gopher_quality_filter(GOOD_TEXT) is True


def test_gopher_rejects_short_documents():
    assert gopher_quality_filter("way too short") is False  # < 50 words


def test_gopher_rejects_extreme_word_lengths():
    assert gopher_quality_filter("a b c d e " * 20) is False  # mean length < 3
    assert gopher_quality_filter("supercalifragilistic " * 60) is False  # mean > 10


def test_gopher_rejects_mostly_ellipsis_lines():
    text = "\n".join(["read more..."] * 7 + [GOOD_TEXT])  # 7/8 lines end with ...
    assert gopher_quality_filter(text) is False


def test_gopher_rejects_mostly_non_alpha():
    assert gopher_quality_filter("1 2 3 4 5 6 7 8 9 10 " * 10) is False


def test_gopher_empty_document():
    assert gopher_quality_filter("") is False


# ---------------------------------------------------------------- exact dedup
def test_exact_line_deduplication_hand_example(tmp_path):
    a = tmp_path / "1.txt"
    b = tmp_path / "2.txt"
    a.write_text("shared header\nunique to a\n")
    b.write_text("shared header\nunique to b\n")
    out = tmp_path / "out"
    exact_line_deduplication([str(a), str(b)], str(out))
    assert (out / "1.txt").read_text() == "unique to a\n"
    assert (out / "2.txt").read_text() == "unique to b\n"


def test_exact_line_deduplication_removes_all_copies(tmp_path):
    a = tmp_path / "1.txt"
    a.write_text("dup\ndup\nkeep\n")
    out = tmp_path / "out"
    exact_line_deduplication([str(a)], str(out))
    # A line occurring twice is removed entirely (not kept once).
    assert (out / "1.txt").read_text() == "keep\n"


# --------------------------------------------------------------- minhash dedup
MIT_A = (
    "MIT License Copyright 2024 Alice. Permission is hereby granted, free of charge, "
    "to any person obtaining a copy of this software and associated documentation files, "
    "to deal in the Software without restriction."
)
MIT_B = (
    "MIT License Copyright 2025 Bob. Permission is hereby granted, free of charge, "
    "to any person obtaining a copy of this software and associated documentation files, "
    "to deal in the Software without restriction."
)
DISTINCT = (
    "A completely different document about cooking pasta with fresh tomatoes and basil, "
    "which shares essentially no phrasing with any license text whatsoever in this corpus."
)


def test_normalize_text():
    assert normalize_text("Héllo,   WORLD!") == "hello world"


def test_word_ngrams_and_jaccard():
    a = _word_ngrams("a b c d", 2)
    assert a == {"a b", "b c", "c d"}
    b = _word_ngrams("a b c e", 2)
    assert _jaccard(a, b) == pytest.approx(2 / 4)


def test_minhash_deduplication_removes_near_duplicates(tmp_path):
    paths = []
    for name, text in [("a.txt", MIT_A), ("b.txt", MIT_B), ("c.txt", DISTINCT)]:
        p = tmp_path / name
        p.write_text(text)
        paths.append(str(p))
    out = tmp_path / "out"
    minhash_deduplication(
        paths, num_hashes=64, num_bands=16, ngram_length=2,
        jaccard_threshold=0.5, output_dir=str(out), seed=0,
    )
    survivors = sorted(p.name for p in out.iterdir())
    # Exactly one of the two near-duplicate licenses survives; distinct doc kept.
    assert "c.txt" in survivors
    assert len([s for s in survivors if s in ("a.txt", "b.txt")]) == 1
    assert len(survivors) == 2


def test_minhash_keeps_unrelated_documents(tmp_path):
    texts = {
        "1.txt": "the cat sat on the mat and looked at the moon during the night hours",
        "2.txt": "quantum computing uses qubits to represent superpositions of binary states",
        "3.txt": "the recipe calls for two cups of flour one egg and a pinch of salt",
    }
    paths = []
    for name, text in texts.items():
        p = tmp_path / name
        p.write_text(text)
        paths.append(str(p))
    out = tmp_path / "out"
    minhash_deduplication(paths, 64, 16, 2, 0.5, str(out))
    assert len(list(out.iterdir())) == 3


# ---------------------------------------------------------------- filter chain
def test_filter_document_chain_with_injected_classifiers():
    counters = Counter()
    keep_all = dict(
        language_fn=lambda t: ("en", 0.99),
        nsfw_fn=lambda t: ("non-nsfw", 0.99),
        toxic_fn=lambda t: ("non-toxic", 0.99),
        quality_fn=lambda t: ("high", 0.9),
    )
    text_with_pii = GOOD_TEXT + " Contact me at someone@example.com."
    kept = filter_document(text_with_pii, counters, **keep_all)
    assert kept is not None
    assert "someone@example.com" not in kept  # PII masked, doc kept
    assert counters["kept"] == 1 and counters["pii_masked"] == 1

    counters2 = Counter()
    dropped = filter_document(GOOD_TEXT, counters2, **{**keep_all, "language_fn": lambda t: ("fr", 0.99)})
    assert dropped is None and counters2["dropped_language"] == 1

    counters3 = Counter()
    dropped = filter_document("too short", counters3, **keep_all)
    assert dropped is None and counters3["dropped_gopher"] == 1

    counters4 = Counter()
    dropped = filter_document(GOOD_TEXT, counters4, **{**keep_all, "nsfw_fn": lambda t: ("nsfw", 0.95)})
    assert dropped is None and counters4["dropped_nsfw"] == 1
