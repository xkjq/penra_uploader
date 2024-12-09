import dicognito.anonymizer
import pydicom
from pathlib import Path
import os

class Anonymizer:

    def __init__(self, output_dir):
        self.output_dir = output_dir
        self.anonymizer = dicognito.anonymizer.Anonymizer()

        self.i: int = 0

    def reset(self):
        self.i = 0
        self.anonymizer = dicognito.anonymizer.Anonymizer()


    def anonymize_file(self, input_file: Path, output_file: Path | None = None, remove_original=True):
        with pydicom.dcmread(input_file) as dataset:

            if output_file is None:
                new_filepath: Path
                # Get next available filename
                while os.path.exists(
                    output_file := self.output_dir.joinpath(f"IMG_{self.i:03}.dcm")
                ):
                    self.i += 1

            self.anonymizer.anonymize(dataset)
            dataset.save_as(output_file)

            if remove_original:
                os.remove(input_file)
            return dataset, output_file


    



