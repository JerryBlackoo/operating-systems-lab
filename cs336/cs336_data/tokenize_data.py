"""Tokenize filtered data with the GPT-2 tokenizer into a uint16 .bin file
(card notes/cs336-a4/10-tokenize-data.md).

    python -m cs336_data.tokenize_data --input filtered.txt --output tokens.bin
"""

from __future__ import annotations

import argparse
import multiprocessing

import numpy as np

_TOKENIZER = None


def _get_tokenizer():
    global _TOKENIZER
    if _TOKENIZER is None:
        from transformers import AutoTokenizer

        _TOKENIZER = AutoTokenizer.from_pretrained("gpt2")
    return _TOKENIZER


def tokenize_line_and_add_eos(line: str) -> list[int]:
    tokenizer = _get_tokenizer()
    return tokenizer.encode(line) + [tokenizer.eos_token_id]  # <|endoftext|> per document


def main(argv=None):
    p = argparse.ArgumentParser()
    p.add_argument("--input", required=True, help="one document per line")
    p.add_argument("--output", required=True, help="raw uint16 .bin (np.fromfile to load)")
    p.add_argument("--workers", type=int, default=multiprocessing.cpu_count())
    p.add_argument("--chunksize", type=int, default=100)
    args = p.parse_args(argv)

    with open(args.input, encoding="utf-8") as f:
        lines = f.readlines()

    if args.workers > 1:
        with multiprocessing.Pool(args.workers) as pool:
            results = list(pool.imap(tokenize_line_and_add_eos, lines, chunksize=args.chunksize))
    else:
        results = [tokenize_line_and_add_eos(line) for line in lines]

    all_ids = [tid for doc in results for tid in doc]
    ids_array = np.array(all_ids, dtype=np.uint16)  # GPT-2 vocab 50257 < 65536
    assert int(ids_array.max()) < 65536
    ids_array.tofile(args.output)
    print(f"wrote {len(ids_array)} tokens to {args.output}")
    return len(ids_array)


if __name__ == "__main__":
    main()
