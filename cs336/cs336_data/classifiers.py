"""fastText-based classifiers: language id, NSFW, toxic speech, quality
(cards notes/cs336-a4/02, 04, 06).

All model loading is lazy and cached. Model files:
- language id: lid.176.bin from fasttext.cc
- NSFW / hate speech: Dolma jigsaw fasttext models
- quality: trained by train_quality_classifier below
"""

from __future__ import annotations

import functools
import os
import random

DEFAULT_LID_PATH = os.environ.get("CS336_LID_MODEL", "/shared-data/classifiers/lid.176.bin")
DEFAULT_NSFW_PATH = os.environ.get(
    "CS336_NSFW_MODEL", "/shared-data/classifiers/dolma_fasttext_nsfw_jigsaw_model.bin"
)
DEFAULT_TOXIC_PATH = os.environ.get(
    "CS336_TOXIC_MODEL", "/shared-data/classifiers/dolma_fasttext_hatespeech_jigsaw_model.bin"
)
DEFAULT_QUALITY_PATH = os.environ.get("CS336_QUALITY_MODEL", "quality_classifier.bin")


@functools.lru_cache(maxsize=8)
def _load_model(path: str):
    import fasttext

    return fasttext.load_model(path)


def _predict(model_path: str, text: str) -> tuple[str, float]:
    model = _load_model(model_path)
    cleaned = text.replace("\n", " ")  # fastText predict() rejects newlines
    labels, probs = model.predict(cleaned)
    label = labels[0].removeprefix("__label__")
    score = min(max(float(probs[0]), 0.0), 1.0)  # clamp fp drift into [0, 1]
    return label, score


def identify_language(text: str, model_path: str = DEFAULT_LID_PATH) -> tuple[str, float]:
    """Returns (language id like 'en'/'zh', confidence in [0,1])."""
    return _predict(model_path, text)


def classify_nsfw(text: str, model_path: str = DEFAULT_NSFW_PATH) -> tuple[str, float]:
    return _predict(model_path, text)


def classify_toxic_speech(text: str, model_path: str = DEFAULT_TOXIC_PATH) -> tuple[str, float]:
    return _predict(model_path, text)


def classify_quality(text: str, model_path: str = DEFAULT_QUALITY_PATH) -> tuple[str, float]:
    return _predict(model_path, text)


# ------------------------------------------------------------ quality classifier
def write_fasttext_training_file(
    positives: list[str], negatives: list[str], output_path: str, seed: int = 0
) -> None:
    """fastText supervised format: one single-line example per row with a label."""
    rng = random.Random(seed)
    rows = [f"__label__high {t}" for t in positives] + [f"__label__low {t}" for t in negatives]
    rng.shuffle(rows)
    with open(output_path, "w", encoding="utf-8") as f:
        for row in rows:
            f.write(row.replace("\n", " ").strip() + "\n")


def train_quality_classifier(
    train_path: str,
    output_model_path: str = DEFAULT_QUALITY_PATH,
    lr: float = 0.1,
    epoch: int = 10,
    word_ngrams: int = 2,
    dim: int = 50,
):
    """Train a fastText quality classifier on a file written by
    write_fasttext_training_file (positives: Wikipedia-linked pages,
    negatives: random Common Crawl pages)."""
    import fasttext

    model = fasttext.train_supervised(
        input=train_path, lr=lr, epoch=epoch, wordNgrams=word_ngrams, dim=dim
    )
    model.save_model(output_model_path)
    return model
