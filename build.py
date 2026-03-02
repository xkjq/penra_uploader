import shutil
from pathlib import Path
import importlib.util
import PyInstaller.__main__
import typer
from rich import print
from rich.progress import Progress
import time

RAD_TOOLS_PATH = Path(r"\\ict\go\RCH\Shared\Dragon-Xray\rad-tools")

app = typer.Typer()

@app.command()
def build(build: bool = True, copy: bool = True, test: bool = False, dest: str | None = None):
    """Windows-focused build script.

    - `--test` creates a one-file test binary in `dist\test`.
    - `--copy` (default) copies the artifact to the shared network path used previously.
    """

    project_root = Path(__file__).resolve().parent

    if build:
        if test:
            print("Building Windows test one-file bundle with PyInstaller")
            # On Windows use ';' as add-data separator (src;dest)
            # Use absolute path for the icon folder so PyInstaller can find it
            icon_src = str(project_root / "icon")
            add_data_args = ["--add-data", f"{icon_src};icon"]

            # Try to include dicognito package data (release_notes.md) which
            # PyInstaller may miss. This prevents runtime FileNotFoundError for
            # dicognito/release_notes.md when the package expects the file at runtime.
            try:
                spec = importlib.util.find_spec("dicognito")
                if spec and spec.submodule_search_locations:
                    dicognito_path = Path(spec.submodule_search_locations[0])
                    release_notes = dicognito_path / "release_notes.md"
                    if release_notes.exists():
                        # PyInstaller on Windows expects 'src;dest'
                        add_data_args += ["--add-data", f"{str(release_notes)};dicognito"]
                    else:
                        # include whole package folder if the single file wasn't found
                        add_data_args += ["--add-data", f"{str(dicognito_path)};dicognito"]
            except Exception as e:
                print(f"Warning: couldn't locate dicognito package: {e}")

            # Try to include pythonnet runtime DLLs to avoid missing
            # Python.Runtime.dll at runtime (used by pythonnet / webview).
            add_binary_args = []
            try:
                spec_py = importlib.util.find_spec("pythonnet")
                if spec_py and spec_py.submodule_search_locations:
                    pn_path = Path(spec_py.submodule_search_locations[0])
                    runtime_dir = pn_path / "runtime"
                    if runtime_dir.exists():
                        for f in runtime_dir.rglob("*.dll"):
                            # PyInstaller on Windows expects 'src;dest'
                            add_binary_args += ["--add-binary", f"{str(f)};pythonnet\\runtime"]
                    # also include the full pythonnet package folder as data so
                    # package-relative lookups succeed at runtime
                    add_data_args += ["--add-data", f"{str(pn_path)};pythonnet"]
                    # try to include clr_loader package as well
                    try:
                        spec_clr = importlib.util.find_spec("clr_loader")
                        if spec_clr and spec_clr.submodule_search_locations:
                            clr_path = Path(spec_clr.submodule_search_locations[0])
                            add_data_args += ["--add-data", f"{str(clr_path)};clr_loader"]
                    except Exception:
                        pass
            except Exception as e:
                print(f"Warning: couldn't locate pythonnet runtime: {e}")

            # Ensure dist/work/spec directories exist
            dist_dir = project_root / "dist" / "test"
            work_dir = project_root / "build" / "test"
            spec_dir = project_root / "build" / "specs"
            dist_dir.mkdir(parents=True, exist_ok=True)
            work_dir.mkdir(parents=True, exist_ok=True)
            spec_dir.mkdir(parents=True, exist_ok=True)

            # Decide build name and try to remove any locked previous artifact.
            base_name = "Uploader_test"
            output_exe = dist_dir / f"{base_name}.exe"

            def try_remove_with_retries(path: Path, retries: int = 5, delay: float = 0.5) -> bool:
                """Try to remove a file with retries and backoff. Returns True if removed or not present."""
                if not path.exists():
                    return True
                for attempt in range(retries):
                    try:
                        path.unlink()
                        print(f"Removed existing file: {path}")
                        return True
                    except PermissionError:
                        # Try to move/rename it out of the way
                        try:
                            backup = path.with_suffix(f".old.{int(time.time())}")
                            path.rename(backup)
                            print(f"Renamed locked file to: {backup}")
                            return True
                        except Exception:
                            pass
                        print(f"File locked, retrying in {delay} seconds ({attempt+1}/{retries})")
                        time.sleep(delay)
                        delay *= 1.5
                    except Exception as e:
                        print(f"Failed to remove existing file {path}: {e}")
                        return False
                return False

            name_used = base_name
            if not try_remove_with_retries(output_exe):
                # If the previous artifact is locked, fall back to a unique name to avoid build failure
                ts = datetime.now().strftime("%Y%m%dT%H%M%S")
                name_used = f"{base_name}_{ts}"
                print(f"Previous artifact appears locked; building as {name_used} to avoid PermissionError")

            # Ensure cert folder is included for test builds as well (if present)
            cert_dir = project_root / "cert"
            if cert_dir.exists() and cert_dir.is_dir():
                arg = f"{str(cert_dir)};cert"
                # Avoid adding duplicate entries
                if arg not in add_data_args:
                    add_data_args += ["--add-data", arg]

            PyInstaller.__main__.run([
                str(project_root / "nice.py"),
                "--onefile",
                "--name",
                name_used,
                "--distpath",
                str(dist_dir),
                "--workpath",
                str(work_dir),
                "--specpath",
                str(spec_dir),
            ] + add_data_args + add_binary_args + [
                # add some common hidden imports that PyInstaller sometimes misses
                "--hidden-import",
                "bs4",
                "--hidden-import",
                "requests",
                "--hidden-import",
                "nicegui",
            ])
        else:
            print("Building using spec: nice_uploader.spec")
            PyInstaller.__main__.run([str(project_root / "nice_uploader.spec")])

    if copy:
        # Windows-only default destination used previously in the repo
        default_dest = Path(r"\\ict\Go\RCH\Shared\Dragon-Xray\rad-tools\uploader\Uploader")

        artifact_dir = project_root / "dist" / ("test" if test else "")
        if not artifact_dir.exists():
            artifact_dir = project_root / "dist"

        # Look for a matching artifact. Prefer exe files named Uploader*.exe,
        # then any .exe, then directories named Uploader* (onedir builds).
        # Exclude obvious non-artifacts like log files.
        candidates = []
        # Uploader*.exe (preferred)
        candidates.extend(sorted(artifact_dir.glob("Uploader*.exe")))
        # any .exe
        candidates.extend(sorted(artifact_dir.glob("*.exe")))
        # onedir: directories starting with Uploader
        candidates.extend([p for p in sorted(artifact_dir.glob("Uploader*")) if p.is_dir()])

        # Filter out files that look like logs or non-executables
        def is_valid_artifact(p: Path) -> bool:
            if not p.exists():
                return False
            if p.is_file():
                # require .exe for files (Windows builds)
                return p.suffix.lower() == ".exe"
            if p.is_dir():
                return True
            return False

        valid = [p for p in candidates if is_valid_artifact(p)]
        if not valid:
            print(f"No build artifact (.exe or Uploader directory) found in {artifact_dir}, skipping copy")
            return

        artifact = valid[0]

        dest_path = Path(dest) if dest else default_dest
        dest_path.mkdir(parents=True, exist_ok=True)

        if artifact.is_file():
            target = dest_path / artifact.name
            print(f"Copying {artifact} -> {target}")
            shutil.copy2(artifact, target)
        else:
            target = dest_path / artifact.name
            print(f"Copying directory {artifact} -> {target}")
            if target.exists():
                shutil.rmtree(target)
            shutil.copytree(artifact, target)


if __name__ == "__main__":
    app()