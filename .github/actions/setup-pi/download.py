"""Download the pinned Pi build tools into the runner's temporary directory."""

import hashlib
import os
import subprocess
from pathlib import Path


def download(url, filename, digest):
    subprocess.run(["curl", "--fail", "--silent", "--show-error", "--location",
                    "--retry", "3", "--output", filename, url], check=True)
    algorithm, expected = digest.split(":")
    with open(filename, "rb") as archive:
        actual = hashlib.file_digest(archive, algorithm).hexdigest()
    if actual != expected:
        raise ValueError(f"{filename}: expected {expected}, got {actual}")


def unpack(url, filename, digest):
    download(url, filename, digest)
    subprocess.run(["tar", "-xzf", filename], check=True)


download("https://github.com/bytecodealliance/wac/releases/download/v0.11.0/wac-cli-x86_64-unknown-linux-musl",
         "wac", "sha256:83259349b630f79a60490322b068e7ad2dba784720b0d467decccc0aecc743b5")
bin_dir = Path("pi-bin").resolve()
bin_dir.mkdir(exist_ok=True)
wac = Path("wac").replace(bin_dir / "wac")
wac.chmod(0o755)
with open(os.environ["GITHUB_PATH"], "a") as path:
    print(bin_dir, file=path)

download("https://registry.npmjs.org/pnpm/-/pnpm-12.9.1.tgz", "pnpm.tgz",
         "sha512:06b055fff5cd20dc12781de173ced3310ce0951bf2ec620211a16045d5444d5c93a663210764ef29a653a610459ba0368f0e74f84d80c54723cf95aae7bc0d6d")
unpack("https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-33/wasi-sdk-33.0-x86_64-linux.tar.gz",
       "wasi-sdk.tgz", "sha256:0ba8b5bfaeb2adf3f29bab5841d76cf5318ab8e1642ea195f88baba1abd47bce")
unpack("https://github.com/WebAssembly/binaryen/releases/download/version_130/binaryen-version_130-x86_64-linux.tar.gz",
       "binaryen.tgz", "sha256:0a18362361ad05465118cd8eeb72edaeec89de6894bc283576ef4e07aa3babcc")
