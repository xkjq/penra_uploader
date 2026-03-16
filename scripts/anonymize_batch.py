#!/usr/bin/env python3
import sys
from pathlib import Path
from anonymiser import Anonymizer

def main():
    if len(sys.argv) < 3:
        print("usage: anonymize_batch.py <input_file> <output_dir> [--remove-original]")
        sys.exit(2)
    input_file = Path(sys.argv[1])
    output_dir = Path(sys.argv[2])
    remove_original = "--remove-original" in sys.argv[3:]
    output_dir.mkdir(parents=True, exist_ok=True)
    a = Anonymizer(output_dir)
    dataset, out = a.anonymize_file(input_file, remove_original=remove_original)
    if out is None:
        sys.exit(1)
    print(str(out))

if __name__ == '__main__':
    main()
