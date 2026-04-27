"""Integration checks for runnable Python examples."""
from __future__ import annotations

import os
import signal
import subprocess
import sys
import time
from pathlib import Path

import pytest


def _repo_root() -> Path:
    return Path(__file__).resolve().parents[4]


def _wait_for_stdout(proc: subprocess.Popen[str], text: str, timeout: float = 20.0) -> str:
    assert proc.stdout is not None
    deadline = time.monotonic() + timeout
    lines: list[str] = []
    while time.monotonic() < deadline:
        line = proc.stdout.readline()
        if line:
            lines.append(line)
            if text in line:
                return "".join(lines)
            continue
        if proc.poll() is not None:
            break
        time.sleep(0.1)
    raise AssertionError(
        f"Timed out waiting for {text!r}; output so far:\n{''.join(lines)}"
    )


def _stop_process(proc: subprocess.Popen[str]) -> None:
    if proc.poll() is not None:
        return
    proc.send_signal(signal.SIGINT)
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)


def _example_env() -> dict[str, str]:
    env = os.environ.copy()
    env["C2_ENV_FILE"] = ""
    env["C2_RELAY_ADDRESS"] = ""
    env["NO_PROXY"] = "127.0.0.1,localhost"
    env["no_proxy"] = "127.0.0.1,localhost"
    env["PYTHONDONTWRITEBYTECODE"] = "1"
    env["PYTHONUNBUFFERED"] = "1"
    return env


def test_relay_client_workflow_uses_explicit_relay_url(start_c3_relay):
    pytest.importorskip("pandas", reason="Python grid examples require examples dependencies")
    pytest.importorskip("pyarrow", reason="Python grid examples require examples dependencies")

    root = _repo_root()
    relay = start_c3_relay()
    relay_url = relay.url

    crm_proc: subprocess.Popen[str] | None = None
    try:
        crm_proc = subprocess.Popen(
            [
                sys.executable,
                str(root / "examples/python/crm_process.py"),
                "--relay-url",
                relay_url,
            ],
            cwd=root,
            env=_example_env(),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        _wait_for_stdout(crm_proc, "Grid CRM registered")

        client = subprocess.run(
            [
                sys.executable,
                str(root / "examples/python/relay_client.py"),
                "--relay-url",
                relay_url,
            ],
            cwd=root,
            env=_example_env(),
            capture_output=True,
            text=True,
            timeout=20,
            check=False,
        )

        assert client.returncode == 0, client.stderr
        assert "[Client] Done." in client.stdout
    finally:
        if crm_proc is not None:
            _stop_process(crm_proc)


def test_general_client_uses_relay_without_ipc_address(start_c3_relay):
    pytest.importorskip("pandas", reason="Python grid examples require examples dependencies")
    pytest.importorskip("pyarrow", reason="Python grid examples require examples dependencies")

    root = _repo_root()
    relay = start_c3_relay()
    relay_url = relay.url

    crm_proc: subprocess.Popen[str] | None = None
    try:
        crm_proc = subprocess.Popen(
            [
                sys.executable,
                str(root / "examples/python/crm_process.py"),
                "--relay-url",
                relay_url,
            ],
            cwd=root,
            env=_example_env(),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        _wait_for_stdout(crm_proc, "Grid CRM registered")

        client = subprocess.run(
            [
                sys.executable,
                str(root / "examples/python/client.py"),
                "--relay-url",
                relay_url,
            ],
            cwd=root,
            env=_example_env(),
            capture_output=True,
            text=True,
            timeout=20,
            check=False,
        )

        assert client.returncode == 0, client.stderr
        assert "Connected (mode:" in client.stdout
        assert "Client done." in client.stdout
    finally:
        if crm_proc is not None:
            _stop_process(crm_proc)


def test_general_client_requires_address_without_relay():
    root = _repo_root()
    client = subprocess.run(
        [
            sys.executable,
            str(root / "examples/python/client.py"),
        ],
        cwd=root,
        env=_example_env(),
        capture_output=True,
        text=True,
        timeout=20,
        check=False,
    )

    assert client.returncode == 2
    assert "address is required" in client.stderr
