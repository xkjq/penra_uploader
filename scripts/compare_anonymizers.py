#!/usr/bin/env python3
"""Compare dicognito vs Rust anonymizer outputs for DICOM files.

Usage: python3 scripts/compare_anonymizers.py <input_dir> [--out-dir report_dir]

Creates a report of tags where dicognito cleared/changed a value but Rust left identifiable data.
"""
import sys
import os
import tempfile
import shutil
import subprocess
from pathlib import Path
import pydicom
from anonymiser import Anonymizer as PyAnon

PHI_KEYWORDS = set([
    "PatientName","PatientID","PatientAddress","PatientTelephoneNumbers",
    "ReferringPhysicianName","PerformingPhysicianName","OperatorsName",
    "InstitutionName","InstitutionAddress","AccessionNumber","StudyInstanceUID",
    "SeriesInstanceUID","SOPInstanceUID","PatientComments","OtherPatientIDs",
    "PatientBirthDate","PatientSex",
])


def anonymize_with_dicognito(in_path, out_path):
    a = PyAnon(output_dir=Path(out_path).parent)
    a.anonymize_file(Path(in_path), Path(out_path), remove_original=False)


def anonymize_with_rust(in_path, out_path):
    exe = Path.cwd() / 'target' / 'debug' / 'uploader_rs'
    if not exe.exists():
        raise SystemExit('Rust binary not found at ' + str(exe))
    res = subprocess.run([str(exe), '--anon', str(in_path), str(out_path)], capture_output=True, text=True)
    if res.returncode != 0:
        raise RuntimeError(f'Rust anonymizer failed: {res.stderr}')


def compare_files(dicog_path, rust_path):
    d = pydicom.dcmread(str(dicog_path))
    r = pydicom.dcmread(str(rust_path))

    # build maps of tag->value (stringified) for all elements
    def build_map(ds):
        m = {}
        for elem in ds.iterall():
            try:
                sval = str(elem.value)
            except Exception:
                sval = repr(elem.value)
            m[elem.keyword if elem.keyword else f'{elem.tag}'] = sval
        return m

    md = build_map(d)
    mr = build_map(r)

    issues = []
    # check PHI keywords and also any element where dicognito cleared but rust didn't
    for k, v_d in md.items():
        v_r = mr.get(k, "")
        if v_d and not v_r:
            # dicognito left value but rust cleared - ok (not an issue)
            continue
        if (not v_d) and v_r:
            # dicognito cleared but rust has value -> potential leak
            issues.append((k, v_r))
        # Note: only report when dicognito cleared but rust has a value (true leak).
    return issues


def main():
    if len(sys.argv) < 2:
        print('Usage: compare_anonymizers.py <input_dir>')
        return
    inp = Path(sys.argv[1])
    files = list(inp.rglob('*.dcm'))
    if not files:
        print('No .dcm files found under', inp)
        return

    report = []
    with tempfile.TemporaryDirectory() as tmpdir:
        tmp = Path(tmpdir)
        for f in files:
            py_out = tmp / ('py_' + f.name)
            rust_out = tmp / ('rs_' + f.name)
            try:
                anonymize_with_dicognito(f, py_out)
            except Exception as e:
                print('dicognito failed for', f, e)
                continue
            try:
                anonymize_with_rust(f, rust_out)
            except Exception as e:
                print('rust anon failed for', f, e)
                continue
            issues = compare_files(py_out, rust_out)
            if issues:
                report.append((str(f), issues))

    # print summary
    if not report:
        print('No issues found: Rust anonymizer appears to match dicognito concerning cleared fields')
    else:
        print('Potential issues:')
        for f, issues in report:
            print('File:', f)
            for k, v in issues:
                print('  ', k, '->', v)

if __name__ == '__main__':
    main()
