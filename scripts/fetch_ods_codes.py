#!/usr/bin/env python3
"""Download NHS Organisation Data Service (ODS) organisation codes.

The anonymiser can use the resulting list to check preserved free-text fields
(StudyDescription, SeriesDescription, ...) for institution identifiers that are
not covered by the standard ODS tags.

Sources
-------
ord
    NHS England Organisation Reference Data (ORD) API. Complete list of
    organisations (including inactive/historical codes), paged 1000 at a time.
    No API key required. This is the simplest way to get every organisation
    code.

dse
    ODS Data Search and Export (DSE) predefined reports, downloaded as CSV.
    More curated (one report per organisation type) and updated nightly. See
    https://digital.nhs.uk/services/organisation-data-service/data-search-and-export/csv-downloads

Output is one code per line, upper-cased, deduplicated and sorted.

ODS data is published by NHS England. Check the applicable licence/attribution
requirements (ODS data is generally available under the Open Government Licence
v3.0) before redistributing the list.

Example
-------
    python scripts/fetch_ods_codes.py --source ord --output ~/.uploader/ods_codes.txt

Note: ODS codes are short (3-9 alphanumeric characters) and some collide with
common clinical abbreviations (for example ADC, DCE, RTP and HFP are all valid
ODS codes as well as radiology terms). Do not blindly redact every token that
appears in this list from free text; use an allow-list or audit-only mode.
"""
from __future__ import annotations

import argparse
import csv
import io
import json
import re
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

USER_AGENT = (
    "penra-uploader-ods-fetch/1.0 "
    "(+https://github.com/xkjq/penra_uploader)"
)

ORD_URL = "https://directory.spineservices.nhs.uk/ORD/2-0-0/organisations"
DSE_URL = "https://www.odsdatasearchandexport.nhs.uk/api/getReport?report={}"

# Organisation-level DSE reports. Individual practitioner/consultant reports
# (egpcur, epracmem, econcur, edconcur, ...) are deliberately excluded: those
# are people, not institution identifiers.
DSE_REPORTS = [
    # GP practices / primary care
    "epraccur", "epcmem", "ebranchs", "epcn",
    # NHS trusts, CCGs, care trusts and Welsh LHBs
    "etr", "ets", "eccg", "eccgsite", "ect", "ectsite", "wlhb", "wlhbsite",
    # Non-NHS
    "ephp", "ephpsite", "enonnhs", "ecarehomehq", "ecarehomesite",
    "eschools", "lauth", "lauthsite", "ejustice",
    # Miscellaneous
    "eopthq", "eoptsite", "egdpprac", "etreat", "ehospice", "eplab",
    "educate", "succ",
]

# ODS codes are 3-9 characters today (some historical ones are longer). This
# also filters out CSV header rows and blank/placeholder values.
CODE_RE = re.compile(r"^[A-Z0-9]{3,12}$")


def fetch(url: str, timeout: int = 60) -> bytes:
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return response.read()


def normalise(value: object) -> str | None:
    """Extract and validate an ODS code, handling the ORD ID object form."""
    if isinstance(value, dict):
        value = value.get("extension")
    if value is None:
        return None
    code = str(value).strip().upper()
    return code if CODE_RE.match(code) else None


def fetch_ord(max_pages: int | None, delay: float) -> set[str]:
    codes: set[str] = set()
    offset: int | None = None
    page = 0
    while True:
        url = ORD_URL + "?Limit=1000"
        if offset:
            url += f"&Offset={offset}"
        try:
            payload = json.loads(fetch(url))
        except (urllib.error.URLError, json.JSONDecodeError) as exc:
            print(f"error: {url}: {exc}", file=sys.stderr)
            break

        organisations = payload.get("Organisations") or []
        if not organisations:
            break

        for organisation in organisations:
            code = normalise(organisation.get("OrgId"))
            if code:
                codes.add(code)

        page += 1
        print(f"  ORD page {page}: {len(codes)} unique codes", file=sys.stderr)
        if max_pages is not None and page >= max_pages:
            break
        offset = (offset or 0) + len(organisations)
        if delay:
            time.sleep(delay)

    return codes


def fetch_dse(delay: float) -> set[str]:
    codes: set[str] = set()
    for report in DSE_REPORTS:
        url = DSE_URL.format(report)
        try:
            raw = fetch(url)
        except urllib.error.URLError as exc:
            print(f"warning: {report}: {exc}", file=sys.stderr)
            continue

        text = raw.decode("utf-8-sig", errors="replace")
        rows = list(csv.reader(io.StringIO(text)))
        for row in rows:
            if not row:
                continue
            code = normalise(row[0])
            if code:
                codes.add(code)
        print(f"  {report}: {len(rows)} rows ({len(codes)} unique so far)", file=sys.stderr)
        if delay:
            time.sleep(delay)

    return codes


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--source",
        choices=["ord", "dse"],
        default="ord",
        help="ord = complete ORD API (default); dse = predefined CSV reports",
    )
    parser.add_argument(
        "--output",
        default="ods_codes.txt",
        help="output file (default: ods_codes.txt)",
    )
    parser.add_argument(
        "--max-pages",
        type=int,
        default=None,
        help="stop after N ORD pages (useful for testing)",
    )
    parser.add_argument(
        "--delay",
        type=float,
        default=0.2,
        help="seconds between requests (default: 0.2)",
    )
    args = parser.parse_args()

    print(f"Downloading ODS codes via {args.source} ...", file=sys.stderr)
    if args.source == "ord":
        codes = fetch_ord(args.max_pages, args.delay)
    else:
        codes = fetch_dse(args.delay)

    if not codes:
        print("error: no codes downloaded", file=sys.stderr)
        return 1

    output = Path(args.output).expanduser()
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text("\n".join(sorted(codes)) + "\n", encoding="utf-8")
    print(f"Wrote {len(codes)} unique ODS codes to {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
