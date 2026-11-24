"""Copy private extension packages from the SAME inventory used by installers."""
import importlib.util
from pathlib import Path


def copy_packages(source,destination):
    source=Path(source)
    spec=importlib.util.spec_from_file_location('_vqro_fixture_packages',source/'vqro_packages.py')
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
    return module.copy_packages(source,Path(destination))
