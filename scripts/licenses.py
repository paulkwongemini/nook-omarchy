#!/usr/bin/env python3
"""Bundle the locked Linux dependency graph's upstream license notices offline."""

import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parent.parent
NOTICE_NAME = re.compile(r"^(?:licen[cs]e|copying|copyright|notice|unlicense)(?:[._-].*)?$", re.I)
# Fail on newly introduced licenses until their actual texts have been reviewed.
LICENSE_MARKERS = {
    "MIT": "to deal in the software without restriction",
    "Apache-2.0": "terms and conditions for use, reproduction, and distribution",
    "Unicode-3.0": "unicode license v3",
    "MPL-2.0": "mozilla public license version 2.0",
    "LLVM-exception": "llvm exceptions to the apache 2.0 license",
    "Zlib": "the origin of this software must not be misrepresented",
    "Unlicense": "this is free and unencumbered software released into the public domain",
}


def command(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def read_notice(path):
    text = path.read_text(encoding="utf-8")
    if not text.strip():
        raise ValueError(f"Empty license or notice file: {path.name}")
    return text


def crate_notices(package):
    root = Path(package["manifest_path"]).resolve().parent
    files = {p for p in root.rglob("*") if p.is_file() and NOTICE_NAME.fullmatch(p.name)}
    if package.get("license_file"):
        declared = root / package["license_file"]
        if not declared.is_file():
            raise ValueError(f"{package['name']}: declared license file is missing")
        files.add(declared)
    notices = []
    for path in sorted(files):
        if not path.resolve().is_relative_to(root):
            raise ValueError(f"{package['name']}: notice file escapes the crate directory")
        notices.append((path.relative_to(root).as_posix(), read_notice(path)))
    if not notices:
        raise ValueError(f"{package['name']}: no upstream license files found")
    expression = package.get("license")
    if not expression:
        raise ValueError(f"{package['name']}: no declared license expression")
    combined = re.sub(r"\s+", " ", "\n".join(text for _, text in notices)).lower()
    identifiers = set(re.findall(r"[A-Za-z0-9][A-Za-z0-9.+-]*", expression)) - {"AND", "OR", "WITH"}
    for identifier in sorted(identifiers):
        marker = LICENSE_MARKERS.get(identifier)
        if marker is None or marker not in combined:
            raise ValueError(f"{package['name']}: missing or unrecognized {identifier} license text")
    return notices


def selected_packages(metadata):
    resolve = metadata["resolve"]
    nodes = {node["id"]: node for node in resolve["nodes"]}
    root = resolve["root"]
    if root is None:
        raise ValueError("Run this script for the Nook package, not a virtual workspace")
    selected = set()
    pending = [root]
    while pending:
        current = pending.pop()
        if current not in selected:
            selected.add(current)
            pending.extend(dep["pkg"] for dep in nodes[current]["deps"])
    return sorted((p for p in metadata["packages"] if p["id"] in selected and p["id"] != root),
                  key=lambda p: (p["name"], p["version"]))


def rust_notices(sysroot):
    candidates = [
        (sysroot / "share/doc/rust/COPYRIGHT-library.html", sysroot / "share/doc/rust/licenses"),
        (sysroot / "share/licenses/rust/COPYRIGHT-library.html.rustc", sysroot / "share/doc/rustc/licenses"),
    ]
    for copyright_file, licenses in candidates:
        if copyright_file.is_file() and licenses.is_dir():
            files = sorted(p for p in licenses.rglob("*") if p.is_file())
            if not files or not (licenses / "MIT.txt").is_file() or not (licenses / "Apache-2.0.txt").is_file():
                continue
            return [(copyright_file.name, read_notice(copyright_file))] + [
                ("licenses/" + p.relative_to(licenses).as_posix(), read_notice(p)) for p in files
            ]
    raise ValueError("Rust standard-library notices are missing; install rustup's rust-docs component "
                     "or use Arch's rust package with its /usr/share/licenses/rust notices")


def section(title, notices):
    output = ["\n" + "=" * 78 + "\n" + title + "\n"]
    for name, text in notices:
        output.append(f"\n--- {name} (upstream text, preserved verbatim) ---\n{text}")
    return "".join(output)


def generate():
    compiler = command("rustc", "-vV")
    fields = dict(line.split(": ", 1) for line in compiler.splitlines() if ": " in line)
    host = fields["host"]
    if "-linux-" not in host:
        raise ValueError("License generation requires a Linux host toolchain")
    metadata = json.loads(command("cargo", "metadata", "--locked", "--offline", "--filter-platform",
                                  host, "--format-version", "1"))
    packages = selected_packages(metadata)
    output = ["Nook for Omarchy - third-party licenses and notices\n\n",
              f"Target: {host}\n",
              "This bundle conservatively includes build dependencies and all upstream license alternatives.\n",
              "System GTK, WebKitGTK and other dynamically linked distribution libraries are not bundled;\n",
              "their notices are supplied by the installed distribution packages.\n"]
    for package in packages:
        source = package.get("source") or ""
        if not source.startswith("registry+") or not any(index in source for index in
                ("crates.io-index", "index.crates.io")):
            raise ValueError(f"{package['name']}: source download URL needs review for this non-crates.io dependency")
        name, version = package["name"], package["version"]
        source_url = f"https://static.crates.io/crates/{name}/{name}-{version}.crate"
        title = f"{name} {version}\nDeclared license: {package.get('license')}\nSource download: {source_url}"
        if package.get("repository"):
            title += f"\nUpstream repository: {package['repository']}"
        if "MPL-2.0" in (package.get("license") or ""):
            title += ("\nSource availability notice: This executable includes this unmodified MPL-2.0 component."
                      "\nIts corresponding Source Code Form, including license notices, is available at: " + source_url)
        output.append(section(title, crate_notices(package)))
    commit = fields["commit-hash"]
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("Cannot identify the exact Rust source revision")
    title = (f"Rust standard library {fields['release']}\n"
             f"Source download: https://github.com/rust-lang/rust/archive/{commit}.tar.gz\n"
             "COPYRIGHT-library.html is preserved as upstream HTML, including embedded dependency notices.\n"
             "The accompanying toolchain license directory is included conservatively in full.")
    output.append(section(title, rust_notices(Path(command("rustc", "--print", "sysroot")))))
    return "".join(output), len(packages)


def main():
    if len(sys.argv) != 2:
        raise ValueError("Usage: python3 scripts/licenses.py OUTPUT_FILE")
    output = Path(sys.argv[1])
    text, count = generate()
    # A failed collection must not replace a previously complete bundle.
    with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=output.parent, delete=False) as temporary:
        temporary.write(text)
        staged = Path(temporary.name)
    try:
        staged.replace(output)
    finally:
        staged.unlink(missing_ok=True)
    print(f"Bundled notices for {count} Cargo dependencies and the Rust standard library.")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"License bundle generation failed: {error}", file=sys.stderr)
        sys.exit(1)
