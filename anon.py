import os

from pathlib import Path

import pydicom
import dicognito.anonymizer

import typer


anonymizer = dicognito.anonymizer.Anonymizer()

app = typer.Typer()


def anonymize_file(filepath: Path, new_filepath: Path):
    with pydicom.dcmread(filepath) as dataset:
        anonymizer.anonymize(dataset)
        dataset.save_as(new_filepath)
        return new_filepath

@app.command()
def annonymise(filepath: Path, output_dir: Path):

    new_filepath : Path
    # Get next available filename
    i : int = 0
    while os.path.exists(new_filepath := Path(output_dir).joinpath(f"IMG_{i:03}.dcm")):
        i += 1

    anonymize_file(filepath, new_filepath)

if __name__ == "__main__":
    app()
