"""Compare cli/Cargo.toml version with GitHub Releases.

Outputs to $GITHUB_OUTPUT:
  should_release=true|false
  version=X.Y.Z
  tag=c3-vX.Y.Z
"""

from __future__ import annotations

import json
import os
import tomllib
import urllib.error
import urllib.request
from pathlib import Path

CLI_MANIFEST_PATH = str(Path(__file__).resolve().parents[2] / "cli" / "Cargo.toml")
GITHUB_API_TIMEOUT = 10
TAG_PREFIX = "c3-v"


def _read_local_version() -> str:
    with open(CLI_MANIFEST_PATH, "rb") as f:
        return tomllib.load(f)["package"]["version"]


def _release_exists(tag: str) -> bool | None:
    repository = os.environ.get("GITHUB_REPOSITORY")
    if not repository:
        return None

    url = f"https://api.github.com/repos/{repository}/releases/tags/{tag}"
    headers = {
        "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
    }
    token = os.environ.get("GITHUB_TOKEN")
    if token:
        headers["Authorization"] = f"Bearer {token}"

    request = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=GITHUB_API_TIMEOUT) as resp:
            data = json.loads(resp.read())
            return data.get("tag_name") == tag
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return False
        return None
    except Exception:
        return None


def main() -> None:
    version = _read_local_version()
    tag = f"{TAG_PREFIX}{version}"
    exists = _release_exists(tag)
    should = exists is False

    with open(os.environ["GITHUB_OUTPUT"], "a") as f:
        f.write(f"should_release={'true' if should else 'false'}\n")
        f.write(f"version={version}\n")
        f.write(f"tag={tag}\n")

    action = "RELEASING" if should else "SKIPPING"
    print(f"[check_cli_release] version={version} tag={tag} exists={exists} -> {action}")


if __name__ == "__main__":
    main()
