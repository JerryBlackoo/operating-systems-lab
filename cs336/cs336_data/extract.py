"""HTML -> text extraction (card notes/cs336-a4/01-extract-text.md).

Requires `resiliparse` (pip install resiliparse). Import is lazy so the rest
of the package works without it.
"""

from __future__ import annotations


def extract_text_from_html_bytes(html_bytes: bytes) -> str:
    """Decode raw HTML bytes (detecting the encoding when UTF-8 fails) and
    extract plain text with resiliparse."""
    from resiliparse.extract.html2text import extract_plain_text
    from resiliparse.parse.encoding import detect_encoding

    try:
        html = html_bytes.decode("utf-8")
    except UnicodeDecodeError:
        encoding = detect_encoding(html_bytes) or "utf-8"
        html = html_bytes.decode(encoding, errors="replace")
    return extract_plain_text(html)


def iter_warc_html_records(warc_path: str):
    """Yield (url, html_bytes) for response records in a (gzipped) WARC file."""
    from fastwarc.warc import ArchiveIterator, WarcRecordType

    with open(warc_path, "rb") as f:
        for record in ArchiveIterator(f, record_types=WarcRecordType.response):
            url = record.headers.get("WARC-Target-URI", "")
            yield url, record.reader.read()


def iter_wet_records(wet_path: str):
    """Yield (url, text) for conversion records in a (gzipped) WET file."""
    from fastwarc.warc import ArchiveIterator, WarcRecordType

    with open(wet_path, "rb") as f:
        for record in ArchiveIterator(f, record_types=WarcRecordType.conversion):
            url = record.headers.get("WARC-Target-URI", "")
            yield url, record.reader.read().decode("utf-8", errors="replace")
