"""CS336 Assignment 4 (data): Common Crawl filtering pipeline.

Implemented following the study cards in notes/cs336-a4/.
Heavy optional dependencies (resiliparse, fasttext, fastwarc) are imported lazily.
"""

from .pii import mask_emails, mask_phone_numbers, mask_ips
from .gopher import gopher_quality_filter
from .dedup import exact_line_deduplication, minhash_deduplication, normalize_text

__all__ = [
    "mask_emails",
    "mask_phone_numbers",
    "mask_ips",
    "gopher_quality_filter",
    "exact_line_deduplication",
    "minhash_deduplication",
    "normalize_text",
]
