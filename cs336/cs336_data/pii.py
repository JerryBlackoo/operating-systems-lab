"""PII masking (card notes/cs336-a4/03-mask-pii.md)."""

from __future__ import annotations

import re

EMAIL_RE = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")

# Common US formats: (650) 555-1234 / 650-555-1234 / 650.555.1234 / 6505551234 /
# +1 650 555 1234, with minor separator variations.
PHONE_RE = re.compile(
    r"""
    (?<![\d.])              # no digit/dot immediately before (avoid long numbers)
    (?:\+?1[\s.\-]?)?       # optional +1 country code
    \(?\d{3}\)?             # area code, optionally parenthesized
    [\s.\-]?\d{3}           # exchange
    [\s.\-]?\d{4}           # subscriber number
    (?![\d.])               # no digit/dot immediately after
    """,
    re.VERBOSE,
)

_OCTET = r"(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)"
IP_RE = re.compile(rf"(?<![\d.])(?:{_OCTET}\.){{3}}{_OCTET}(?![\d.])")


def mask_emails(text: str) -> tuple[str, int]:
    return EMAIL_RE.subn("|||EMAIL_ADDRESS|||", text)


def mask_phone_numbers(text: str) -> tuple[str, int]:
    return PHONE_RE.subn("|||PHONE_NUMBER|||", text)


def mask_ips(text: str) -> tuple[str, int]:
    return IP_RE.subn("|||IP_ADDRESS|||", text)


def mask_all_pii(text: str) -> tuple[str, dict[str, int]]:
    text, n_email = mask_emails(text)
    text, n_phone = mask_phone_numbers(text)
    text, n_ip = mask_ips(text)
    return text, {"emails": n_email, "phones": n_phone, "ips": n_ip}
