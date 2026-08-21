"""Parallel filtering pipeline (card notes/cs336-a4/09-filter-data.md).

Chains the primitives: cheap rules first, expensive classifiers last, and
reports per-filter keep/drop counters. Requires fastwarc + fasttext models
for the full run; the chain itself (filter_document) is dependency-light and
unit-testable with injected classifier functions.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
from collections import Counter
from pathlib import Path

from .gopher import gopher_quality_filter
from .pii import mask_all_pii


def filter_document(
    text: str,
    counters: Counter,
    language_fn=None,
    nsfw_fn=None,
    toxic_fn=None,
    quality_fn=None,
    min_language_score: float = 0.8,
    max_nsfw_score: float = 0.8,
    max_toxic_score: float = 0.8,
    min_quality_score: float = 0.5,
) -> str | None:
    """Run one document through the filter chain. Returns the (PII-masked)
    text if kept, or None if any filter drops it. Order: cheap -> expensive."""
    counters["total"] += 1

    if not gopher_quality_filter(text):
        counters["dropped_gopher"] += 1
        return None

    if language_fn is not None:
        lang, score = language_fn(text)
        if lang != "en" or score < min_language_score:
            counters["dropped_language"] += 1
            return None

    if nsfw_fn is not None:
        label, score = nsfw_fn(text)
        if label == "nsfw" and score > max_nsfw_score:
            counters["dropped_nsfw"] += 1
            return None

    if toxic_fn is not None:
        label, score = toxic_fn(text)
        if label == "toxic" and score > max_toxic_score:
            counters["dropped_toxic"] += 1
            return None

    if quality_fn is not None:
        label, score = quality_fn(text)
        if label != "high" or score < min_quality_score:
            counters["dropped_quality"] += 1
            return None

    text, pii_counts = mask_all_pii(text)  # masking modifies, never drops
    counters["pii_masked"] += sum(pii_counts.values())
    counters["kept"] += 1
    return text


def process_single_wet_file(input_path: str, output_path: str, use_classifiers: bool = True):
    """Filter one WET file; one output document per line block separated by blank lines."""
    from .extract import iter_wet_records

    language_fn = nsfw_fn = toxic_fn = quality_fn = None
    if use_classifiers:
        from .classifiers import classify_nsfw, classify_quality, classify_toxic_speech, identify_language

        language_fn, nsfw_fn = identify_language, classify_nsfw
        toxic_fn, quality_fn = classify_toxic_speech, classify_quality

    counters: Counter = Counter()
    with open(output_path, "w", encoding="utf-8") as out:
        for _url, text in iter_wet_records(input_path):
            kept = filter_document(
                text, counters,
                language_fn=language_fn, nsfw_fn=nsfw_fn,
                toxic_fn=toxic_fn, quality_fn=quality_fn,
            )
            if kept is not None:
                out.write(kept.replace("\n", " ").strip() + "\n")
    return output_path, counters


def main(argv=None):  # pragma: no cover - needs WET files + classifier models
    p = argparse.ArgumentParser(description="Filter Common Crawl WET files in parallel")
    p.add_argument("--input-dir", required=True)
    p.add_argument("--output-dir", required=True)
    p.add_argument("--no-classifiers", action="store_true")
    p.add_argument("--max-workers", type=int, default=len(os.sched_getaffinity(0)))
    args = p.parse_args(argv)

    os.makedirs(args.output_dir, exist_ok=True)
    wet_paths = sorted(str(p) for p in Path(args.input_dir).glob("*.warc.wet.gz"))
    totals: Counter = Counter()
    with concurrent.futures.ProcessPoolExecutor(max_workers=args.max_workers) as executor:
        futures = [
            executor.submit(
                process_single_wet_file,
                path,
                os.path.join(args.output_dir, Path(path).stem + ".txt"),
                not args.no_classifiers,
            )
            for path in wet_paths
        ]
        for future in concurrent.futures.as_completed(futures):
            output_file, counters = future.result()
            totals.update(counters)
            print(f"done {output_file}: {dict(counters)}")
    print("TOTALS:", json.dumps(dict(totals), indent=2))


if __name__ == "__main__":
    main()
