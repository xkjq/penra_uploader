from click.core import F
import dicognito.anonymizer
import pydicom
from pathlib import Path
import os

class Anonymizer:

    def __init__(self, output_dir, seed: str | None = None):
        self.output_dir = output_dir
        self.anonymizer = dicognito.anonymizer.Anonymizer(seed=seed)


        self.i: int = 0

    def reset(self):
        self.i = 0
        self.anonymizer = dicognito.anonymizer.Anonymizer()


    def anonymize_file(self, input_file: Path, output_file: Path | None = None, remove_original=True):

        try:
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
        except FileNotFoundError:
            print(f"Input file not found: {input_file}")
            return None, None
        except Exception as e:
            print(f"Error anonymizing file {input_file}: {e}")
            return None, None


