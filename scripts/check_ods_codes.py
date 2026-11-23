#!/usr/bin/env python3
"""Check DICOM headers for ODS organisation codes.

This is an audit tool: it reports codes that survived anonymisation rather than
redacting them, because many short ODS codes collide with clinical terms (for
example ADC, DCE, RTP and HFP are all valid ODS codes as well as radiology
terms).

It scans textual elements (including nested sequences, but not pixel data) for
tokens that appear in the supplied code list, and also reports any of the
standard ODS-bearing tags that are still present. A leading ODS code in an
identifier is detected too (for example AccessionNumber ``RGT01123456``),
unless ``--no-prefix`` is given.

Usage
-----
    python scripts/fetch_ods_codes.py --output ~/.uploader/ods_codes.txt
    python scripts/check_ods_codes.py --codes ~/.uploader/ods_codes.txt anon/

Exit status
-----------
    0  no ODS codes / ODS tags found
    1  findings (codes or ODS-bearing tags present)
    2  could not run (missing code list, etc.)
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

try:
    import pydicom
except ImportError:  # pragma: no cover - dependency hint
    print("error: pydicom is required (pip install pydicom)", file=sys.stderr)
    raise SystemExit(2)

# Free-text VRs that can carry organisation identifiers. UIDs and numeric/date
# VRs are excluded to avoid false positives.
TEXT_VRS = {"AE", "AS", "CS", "LO", "LT", "PN", "SH", "ST", "UC", "UT"}

# Standard ODS / assigning-authority fields. These should normally be absent
# after anonymisation.
ODS_TAGS = {
    (0x0008, 0x0082): "InstitutionCodeSequence",
    (0x0010, 0x0024): "IssuerOfPatientIDQualifiersSequence",
    (0x0040, 0x0031): "LocalEntity",
    (0x0040, 0x0032): "UniversalEntityID",
    (0x0040, 0x0033): "UniversalEntityIDType",
}

TOKEN_RE = re.compile(r"[A-Za-z0-9]+")
DICOM_EXTENSIONS = {".dcm", ".dicom", ".ima"}


def load_codes(path: Path) -> set[str]:
    codes: set[str] = set()
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            line = line.strip().upper()
            if line and not line.startswith("#"):
                codes.add(line)
    return codes


def is_match(token: str, codes: set[str], min_length: int, include_all: bool) -> bool:
    """Avoid noisy three-digit hits such as house numbers and ID fragments.

    By default a code must be at least ``min_length`` characters or contain a
    letter. Pure 3-4 digit ODS codes (local authority/successor codes) are
    still available with ``--all``.
    """
    if token not in codes:
        return False
    if include_all:
        return True
    return len(token) >= min_length or any(ch.isalpha() for ch in token)


def iter_elements(dataset):
    for element in dataset:
        yield element
        if element.VR == "SQ":
            try:
                for item in element.value:
                    yield from iter_elements(item)
            except TypeError:
                continue


def textual_values(element):
    if element.VR not in TEXT_VRS:
        return []
    value = element.value
    if isinstance(value, (list, tuple)):
        return [str(v) for v in value if v is not None]
    if value is None:
        return []
    return [str(value)]


def find_code(
    token: str,
    codes: set[str],
    min_length: int,
    include_all: bool,
    min_code_len: int,
    max_code_len: int,
    prefix: bool,
) -> tuple[str | None, str | None]:
    """Return ``(code, kind)`` for a token, checking for a leading ODS code.

    Accession numbers and similar identifiers are often the site ODS code
    followed by digits (``RGT01123456``), so a prefix match is checked after
    an exact match. The longest matching prefix wins.
    """
    upper = token.upper()
    if is_match(upper, codes, min_length, include_all):
        return upper, "exact"
    if prefix:
        upper_bound = min(max_code_len, len(upper))
        for length in range(upper_bound, min_code_len - 1, -1):
            candidate = upper[:length]
            if is_match(candidate, codes, min_length, include_all):
                return candidate, "prefix"
    return None, None


def scan_file(
    path: Path,
    codes: set[str],
    min_length: int,
    include_all: bool,
    min_code_len: int,
    max_code_len: int,
    prefix: bool,
) -> dict:
    dataset = pydicom.dcmread(path, stop_before_pixels=True, force=False)
    findings = []
    ods_tags = []

    for element in iter_elements(dataset):
        key = (element.tag.group, element.tag.element)
        if key in ODS_TAGS:
            ods_tags.append(
                {
                    "file": str(path),
                    "tag": f"({key[0]:04X},{key[1]:04X})",
                    "keyword": ODS_TAGS[key],
                    "vr": element.VR,
                }
            )
            continue

        for value in textual_values(element):
            for token in TOKEN_RE.findall(value):
                code, kind = find_code(
                    token,
                    codes,
                    min_length,
                    include_all,
                    min_code_len,
                    max_code_len,
                    prefix,
                )
                if code is not None:
                    findings.append(
                        {
                            "file": str(path),
                            "tag": f"({element.tag.group:04X},{element.tag.element:04X})",
                            "keyword": element.keyword or "",
                            "vr": element.VR,
                            "code": code,
                            "match": kind,
                            "value": value,
                        }
                    )

    return {"findings": findings, "ods_tags": ods_tags}


def iter_files(paths: list[Path]):
    for path in paths:
        if path.is_dir():
            for candidate in sorted(path.rglob("*")):
                if candidate.is_file() and candidate.suffix.lower() in DICOM_EXTENSIONS:
                    yield candidate
        elif path.is_file():
            yield path


def main() -> int:
    parser = argparse.ArgumentParser(description="Check DICOM headers for ODS codes")
    parser.add_argument(
        "--codes",
        default="ods_codes.txt",
        help="ODS code list, one per line (default: ods_codes.txt)",
    )
    parser.add_argument("paths", nargs="+", help="DICOM files or directories")
    parser.add_argument("--json", action="store_true", help="emit JSON")
    parser.add_argument("--quiet", action="store_true", help="only print findings")
    parser.add_argument(
        "--min-length",
        type=int,
        default=5,
        help="minimum length for a pure-numeric match (default: 5); codes with letters always match",
    )
    parser.add_argument(
        "--all",
        action="store_true",
        help="match every listed code, including short pure-numeric ones (noisier)",
    )
    parser.add_argument(
        "--no-prefix",
        dest="prefix",
        action="store_false",
        help="only match whole tokens; do not detect a leading ODS code (e.g. RGT01123456)",
    )
    args = parser.parse_args()

    codes_path = Path(args.codes).expanduser()
    if not codes_path.is_file():
        print(f"error: code list not found: {codes_path}", file=sys.stderr)
        print("hint: run scripts/fetch_ods_codes.py first", file=sys.stderr)
        return 2

    codes = load_codes(codes_path)
    if not codes:
        print(f"error: code list is empty: {codes_path}", file=sys.stderr)
        return 2

    min_code_len = max(3, min(len(code) for code in codes))
    max_code_len = max(len(code) for code in codes)

    all_findings = []
    all_tags = []
    errors = []
    checked = 0

    for path in iter_files([Path(p).expanduser() for p in args.paths]):
        try:
            result = scan_file(
                path,
                codes,
                args.min_length,
                args.all,
                min_code_len,
                max_code_len,
                args.prefix,
            )
        except Exception as exc:  # malformed/non-DICOM files
            errors.append({"file": str(path), "error": str(exc)})
            continue
        checked += 1
        all_findings.extend(result["findings"])
        all_tags.extend(result["ods_tags"])

    if args.json:
        print(
            json.dumps(
                {
                    "codes_file": str(codes_path),
                    "files_checked": checked,
                    "findings": all_findings,
                    "ods_tags": all_tags,
                    "errors": errors,
                },
                indent=2,
            )
        )
    else:
        for item in all_tags:
            print(
                f"{item['file']}: ODS-bearing tag {item['tag']} "
                f"{item['keyword']} ({item['vr']}) is present"
            )
        for item in all_findings:
            print(
                f"{item['file']}: ODS code {item['code']} ({item['match']}) in "
                f"{item['tag']} {item['keyword']} ({item['vr']}): {item['value']!r}"
            )
        for item in errors:
            print(f"{item['file']}: could not read: {item['error']}", file=sys.stderr)
        if not args.quiet:
            print(
                f"Checked {checked} file(s): {len(all_findings)} code finding(s), "
                f"{len(all_tags)} ODS tag(s), {len(errors)} unreadable file(s).",
                file=sys.stderr,
            )

    return 1 if (all_findings or all_tags) else 0


if __name__ == "__main__":
    raise SystemExit(main())
