"""A1 cards 01-02: BPE training + Tokenizer."""

import pytest

from cs336_basics.bpe import train_bpe, save_vocab_and_merges
from cs336_basics.tokenizer import Tokenizer


def _train_on_text(tmp_path, text, vocab_size, special_tokens):
    path = tmp_path / "corpus.txt"
    path.write_text(text, encoding="utf-8")
    return train_bpe(str(path), vocab_size, special_tokens)


def test_bpe_hand_example(tmp_path):
    # "aaabdaaabac" is a single pre-token under the GPT-2 regex.
    # pairs: (a,a) x4, (a,b) x2, ... -> merge (a,a)
    # then (aa,a) x2 ties (a,b) x2; lexicographically greater is (aa,a) -> merge
    # then (aaa,b) x2 -> merge
    vocab, merges = _train_on_text(tmp_path, "aaabdaaabac", 256 + 3, [])
    assert merges == [(b"a", b"a"), (b"aa", b"a"), (b"aaa", b"b")]
    assert set(vocab.values()) >= {b"aa", b"aaa", b"aaab"}
    assert len(vocab) == 256 + 3


def test_bpe_special_tokens_are_boundaries(tmp_path):
    text = "ab<|endoftext|>ab<|endoftext|>"
    vocab, merges = _train_on_text(tmp_path, text, 256 + 1 + 1, ["<|endoftext|>"])
    # (a,b) occurs twice; nothing merges across the special token.
    assert merges == [(b"a", b"b")]
    assert b"<|endoftext|>" in vocab.values()
    # No merged token may contain any part of the special token's text next to 'b'.
    assert b"b<" not in vocab.values()


def test_bpe_vocab_size_accounting(tmp_path):
    text = "the quick brown fox jumps over the lazy dog " * 20
    vocab, merges = _train_on_text(tmp_path, text, 280, ["<|endoftext|>"])
    assert len(vocab) == 280
    assert len(merges) == 280 - 256 - 1
    # When the corpus runs out of pairs, training stops early instead of crashing.
    vocab_big, merges_big = _train_on_text(tmp_path, text, 400, ["<|endoftext|>"])
    assert len(vocab_big) < 400
    assert len(vocab_big) == 256 + 1 + len(merges_big)


def test_handout_encoding_example():
    # Straight from the handout (bpe_encoding example).
    vocab = {
        0: b" ", 1: b"a", 2: b"c", 3: b"e", 4: b"h", 5: b"t",
        6: b"th", 7: b" c", 8: b" a", 9: b"the", 10: b" at",
    }
    merges = [(b"t", b"h"), (b" ", b"c"), (b" ", b"a"), (b"th", b"e"), (b" a", b"t")]
    tokenizer = Tokenizer(vocab, merges)
    assert tokenizer.encode("the cat ate") == [9, 7, 1, 5, 10, 3]


def test_roundtrip_with_trained_tokenizer(tmp_path):
    text = (
        "low low low low low\nlower lower widest widest widest\n"
        "newest newest newest newest newest newest\n"
    )
    vocab, merges = _train_on_text(tmp_path, text * 5, 280, ["<|endoftext|>"])
    tok = Tokenizer(vocab, merges, ["<|endoftext|>"])
    sample = "the newest widest low thing<|endoftext|>and lower ones\n"
    assert tok.decode(tok.encode(sample)) == sample


def test_special_token_encodes_to_single_id(tmp_path):
    vocab, merges = _train_on_text(tmp_path, "hello world " * 10, 270, ["<|endoftext|>"])
    tok = Tokenizer(vocab, merges, ["<|endoftext|>"])
    ids = tok.encode("hello<|endoftext|>world")
    special_id = tok.bytes_to_id[b"<|endoftext|>"]
    assert ids.count(special_id) == 1


def test_overlapping_special_tokens():
    vocab = {i: bytes([i]) for i in range(256)}
    specials = ["<|endoftext|>", "<|endoftext|><|endoftext|>"]
    tok = Tokenizer(vocab, [], specials)
    double_id = tok.bytes_to_id["<|endoftext|><|endoftext|>".encode()]
    single_id = tok.bytes_to_id[b"<|endoftext|>"]
    ids = tok.encode("x<|endoftext|><|endoftext|>x")
    # Longest special must win: exactly one double token, no single tokens.
    assert ids.count(double_id) == 1
    assert ids.count(single_id) == 0


def test_encode_iterable_matches_encode(tmp_path):
    text = "hello world\nthis is a test\nof the iterable encoder\n"
    vocab, merges = _train_on_text(tmp_path, text * 3, 270, [])
    tok = Tokenizer(vocab, merges)
    lines = text.splitlines(keepends=True)
    assert list(tok.encode_iterable(iter(lines))) == tok.encode(text)


def test_decode_invalid_utf8_uses_replacement_char():
    vocab = {i: bytes([i]) for i in range(256)}
    tok = Tokenizer(vocab, [])
    # 0xC3 alone is an invalid UTF-8 sequence.
    assert tok.decode([0xC3]) == "\ufffd"


def test_from_files_roundtrip(tmp_path):
    vocab, merges = _train_on_text(tmp_path, "some text to train on " * 10, 265, [])
    vp, mp = tmp_path / "vocab.pkl", tmp_path / "merges.pkl"
    save_vocab_and_merges(vocab, merges, vp, mp)
    tok = Tokenizer.from_files(vp, mp)
    assert tok.encode("some text") == Tokenizer(vocab, merges).encode("some text")
