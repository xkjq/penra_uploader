#!/usr/bin/env python3
import sys
from pathlib import Path
from anonymiser import Anonymizer


def main():
    if len(sys.argv) < 3:
        print("usage: anonymize_dir.py <input_dir> <output_dir> [--remove-original]")
        sys.exit(2)
    input_dir = Path(sys.argv[1])
    output_dir = Path(sys.argv[2])
    remove_original = "--remove-original" in sys.argv[3:]
    output_dir.mkdir(parents=True, exist_ok=True)
    a = Anonymizer(output_dir)
    processed = []
    for p in sorted(input_dir.glob("**/*.dcm")):
        dataset, out = a.anonymize_file(p, remove_original=remove_original)
        if out:
            print(str(out))
            processed.append(str(out))

    # print summary to stdout
    print(f"processed:{len(processed)}")


if __name__ == '__main__':
    main()
