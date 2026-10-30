"""Builds Retool for Selection from the vendored tree, its patches and the pinned clone lists.

Standard library only, so it runs before the venv exists and in the image's builder stage.

    build.py build --dest <dir> [--runtime-only] [--no-pristine]
    build.py pin-clone-lists
    build.py hash-tree

`build` verifies every pin in third_party/retool/pins.toml, then writes:

    <dest>/retool/      the patched Retool, with internal-config.json and the pinned clone
                        lists, metadata, MIAs and RetroAchievements beside it
    <dest>/pristine/    the unpatched Retool, for the patch-neutrality test
    <dest>/pins.json    the resolved pins every Selection reports
    <dest>/cache/       the fetched clone-list archive, reused while its sha256 matches

`pin-clone-lists` fetches the archive for the clone-list commit in pins.toml and writes
its sha256 there. `hash-tree` prints the vendored tree's and the patches' hashes, for a
deliberate change to the Retool pin.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tomllib
import urllib.request
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parents[2]
RETOOL = ROOT / 'third_party' / 'retool'
PINS = RETOOL / 'pins.toml'

# The files that run. The rest of the vendored tree (tests, docs) is for conformance and reference.
RUNTIME = ('LICENSE', 'retool.py', 'modules')
DATA_DIRS = ('clonelists', 'metadata', 'mias', 'retroachievements')


def fail(message: str) -> None:
    sys.exit(f'retool build: {message}')


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open('rb') as file:
        for chunk in iter(lambda: file.read(1 << 20), b''):
            digest.update(chunk)
    return digest.hexdigest()


def tree_sha256(root: Path, runtime_only: bool = False) -> str:
    """sha256 over `<file sha256>  <relative path>` lines for every file, sorted by path."""
    lines = []
    for path in root.rglob('*'):
        relative = PurePosixPath(path.relative_to(root).as_posix())
        if runtime_only and relative.parts[0] not in RUNTIME:
            continue
        if '__pycache__' in relative.parts or not path.is_file():
            continue
        lines.append((str(relative), f'{sha256_file(path)}  {relative}\n'))
    return hashlib.sha256(''.join(line for _, line in sorted(lines)).encode()).hexdigest()


def archive_url(clone_lists: dict) -> str:
    return f'{clone_lists["repository"]}/archive/{clone_lists["commit"]}.tar.gz'


def fetch(url: str) -> bytes:
    with urllib.request.urlopen(url, timeout=120) as response:  # noqa: S310
        return response.read()


def build(dest: Path, runtime_only: bool, pristine: bool) -> None:
    pins = tomllib.loads(PINS.read_text())
    upstream = RETOOL / 'upstream'

    key = 'runtime_sha256' if runtime_only else 'tree_sha256'
    if (actual := tree_sha256(upstream, runtime_only)) != pins['retool'][key]:
        fail(f'third_party/retool/upstream does not match pins.toml {key} ({actual})')

    patches = []
    for patch in pins['patches']:
        path = RETOOL / 'patches' / patch['file']
        if (actual := sha256_file(path)) != patch['sha256']:
            fail(f'{path.name} does not match its sha256 in pins.toml ({actual})')
        patches.append(path)
    if sorted(p.name for p in (RETOOL / 'patches').glob('*.patch')) != [p.name for p in patches]:
        fail('pins.toml must list every patch in third_party/retool/patches, in order')
    patch_set = hashlib.sha256(''.join(p['sha256'] for p in pins['patches']).encode()).hexdigest()

    clone_lists = pins['clone_lists']
    cache = dest / 'cache' / f'clone-lists-{clone_lists["commit"]}.tar.gz'
    if not cache.is_file() or sha256_file(cache) != clone_lists['archive_sha256']:
        data = fetch(archive_url(clone_lists))
        if (actual := hashlib.sha256(data).hexdigest()) != clone_lists['archive_sha256']:
            fail(f'the clone-list archive does not match pins.toml archive_sha256 ({actual})')
        cache.parent.mkdir(parents=True, exist_ok=True)
        cache.write_bytes(data)

    variants = [('retool', True)] + ([('pristine', False)] if pristine else [])
    for name, patched in variants:
        target = dest / name
        shutil.rmtree(target, ignore_errors=True)
        target.mkdir(parents=True)
        for item in RUNTIME:
            source = upstream / item
            if source.is_dir():
                shutil.copytree(source, target / item, ignore=shutil.ignore_patterns('__pycache__'))
            else:
                shutil.copy2(source, target / item)
        if patched:
            for patch in patches:
                # The ceiling stops git from treating a surrounding checkout as the repository.
                subprocess.run(
                    ['git', 'apply', '--whitespace=nowarn', str(patch)],
                    cwd=target,
                    check=True,
                    env={**os.environ, 'GIT_CEILING_DIRECTORIES': str(target.parent)},
                )
        extract(cache, target, with_data=patched)

    (dest / 'pins.json').write_text(
        json.dumps(
            {
                'retool_commit': pins['retool']['commit'],
                'patch_set_sha256': patch_set,
                'clone_list_commit': clone_lists['commit'],
            },
            indent=2,
        )
        + '\n'
    )


def extract(archive: Path, target: Path, with_data: bool) -> None:
    """Unpacks internal-config.json and, for the patched install, the clone-list data and its licence."""
    wanted = {'config', 'LICENSE', *DATA_DIRS} if with_data else {'config'}
    with tarfile.open(archive) as tar:
        for member in tar.getmembers():
            relative = PurePosixPath(*PurePosixPath(member.name).parts[1:])
            if not member.isfile() or not relative.parts or relative.parts[0] not in wanted:
                continue
            if relative.parts[0] == 'config' and relative.name != 'internal-config.json':
                continue
            if relative == PurePosixPath('LICENSE'):
                relative = PurePosixPath('clonelists-LICENSE')
            out = target / relative
            out.parent.mkdir(parents=True, exist_ok=True)
            source = tar.extractfile(member)
            assert source is not None
            out.write_bytes(source.read())
    # Retool refuses to start without these, even when it reads its test data instead.
    for directory in ('clonelists', 'metadata'):
        (target / directory).mkdir(exist_ok=True)


def pin_clone_lists() -> None:
    text = PINS.read_text()
    clone_lists = tomllib.loads(text)['clone_lists']
    digest = hashlib.sha256(fetch(archive_url(clone_lists))).hexdigest()
    updated, count = re.subn(r'(?m)^archive_sha256 = "[0-9a-f]*"$', f'archive_sha256 = "{digest}"', text)
    if count != 1:
        fail('pins.toml needs exactly one archive_sha256 line')
    PINS.write_text(updated)
    print(f'clone-list archive for {clone_lists["commit"]}: {digest}')


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    commands = parser.add_subparsers(dest='command', required=True)
    build_command = commands.add_parser('build')
    build_command.add_argument('--dest', type=Path, required=True)
    build_command.add_argument(
        '--runtime-only', action='store_true', help='verify only the files that run, as the image does'
    )
    build_command.add_argument('--no-pristine', action='store_true')
    commands.add_parser('pin-clone-lists')
    commands.add_parser('hash-tree')
    args = parser.parse_args()

    if args.command == 'build':
        build(args.dest.resolve(), args.runtime_only, not args.no_pristine)
    elif args.command == 'pin-clone-lists':
        pin_clone_lists()
    else:
        upstream = RETOOL / 'upstream'
        print(f'tree_sha256 = "{tree_sha256(upstream)}"')
        print(f'runtime_sha256 = "{tree_sha256(upstream, runtime_only=True)}"')
        for patch in sorted((RETOOL / 'patches').glob('*.patch')):
            print(f'{patch.name} {sha256_file(patch)}')


if __name__ == '__main__':
    main()
