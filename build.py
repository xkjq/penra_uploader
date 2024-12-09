import shutil
from pathlib import Path
import PyInstaller.__main__
import typer
from rich import print
from rich.progress import Progress


app = typer.Typer()

@app.command()
def build(build: bool = True, copy: bool = True, test: bool = False):

    if build:

        print("Building with PyInstaller")
        PyInstaller.__main__.run([
            "anon_gui.spec"
        ])


    if copy:
        print("Copying file")
        source = Path(r"C:\Users\krugerro\Desktop\uploader\dist\anon_gui.exe" )
        dest =Path(r"T:\rad-tools\uploader" )



        n = 0
        while True:
            if n > 5: 
                break

            s = n if n > 0 else ""
            filename = Path(f"cris_tools{s}.exe")

            print(f"Destination: {filename}")
            try:
                shutil.copy(source, dest / filename)
                break

            except PermissionError:
                n = n + 1


if __name__ == "__main__":
    app()