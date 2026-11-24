"""One shipped Python-package inventory for installers and private fixtures.

Code/data only: never enumerate native settings, sessions or credentials.
"""
import argparse
import json
from pathlib import Path
import shutil

PACKAGE_ASSETS={'pi_accounts':('index.mjs','provider.mjs','limit_messages.json')}


def package_files(root,package=None):
    root=Path(root)
    result=[]
    for directory in sorted(root.iterdir()):
        if not directory.is_dir() or not (directory/'__init__.py').is_file():continue
        if package is not None and directory.name!=package:continue
        files=sorted(directory.glob('*.py'))
        files += [directory/name for name in PACKAGE_ASSETS.get(directory.name,())]
        for source in files:
            if not source.is_file():raise ValueError('missing shipped package file: '+str(source))
            result.append(source.relative_to(root))
    if package is not None and not result:raise ValueError('unknown shipped Python package: '+package)
    return result


def copy_packages(root,destination):
    root=Path(root);destination=Path(destination)
    files=package_files(root)  # Validate the WHOLE source inventory before copying.
    for relative in files:
        target=destination/relative;target.parent.mkdir(parents=True,exist_ok=True)
        shutil.copy2(root/relative,target)
    return files


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root',type=Path)
    parser.add_argument('--package')
    parser.add_argument('--copy-to',type=Path)
    parser.add_argument('--check',action='store_true')
    args=parser.parse_args()
    if args.copy_to is not None:
        if args.package is not None:parser.error('--package is only for inventory listing')
        copy_packages(args.root,args.copy_to)
    else:
        files=package_files(args.root,args.package)
        if not args.check:print(json.dumps([str(path) for path in files]))


if __name__=='__main__':main()
