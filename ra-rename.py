#!/usr/bin/env python3
"""Semantic rename for Rust, driven through rust-analyzer's LSP.

rust-analyzer's command line has no `rename`: the operation exists only as the LSP request
`textDocument/rename`, which is what an editor calls. This is a minimal client for that one
request, so a rename can be done by meaning rather than by text substitution.

    ra-rename.py <workspace> <file>:<line>:<col> <new-name> [--dry-run]
    ra-rename.py <workspace> <file>@<symbol>    <new-name> [--dry-run]

The second form finds the first whole-word occurrence of <symbol> in <file> and renames there,
which is usually what you want for a module: point at its `mod foo;` declaration.

    ra-rename.py . src/lib.rs@sessions session

What it covers and what it does not
-----------------------------------
It renames the definition and every reference the compiler can see, and it moves the module's file
or directory when the server asks for that. It does not touch anything outside Rust's view: prose
in comments, markdown, directory names other than the module's own, or paths inside strings. Check
those afterwards -- that is where the work actually is.

Exit status is 0 when a rename was applied (or planned, under --dry-run), 1 otherwise.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path
from urllib.parse import quote, unquote, urlparse

# rust-analyzer has to load and index the crate before it can resolve anything. Renaming too early
# returns an empty edit rather than an error, which would read as "nothing to do" instead of "not
# ready" -- so the wait is not optional.
INDEX_TIMEOUT_S = 300
REQUEST_TIMEOUT_S = 120


def uri_of(path: Path) -> str:
    return "file://" + quote(str(path.resolve()))


def path_of(uri: str) -> Path:
    return Path(unquote(urlparse(uri).path))


def utf16_len(s: str) -> int:
    """Length in UTF-16 code units, which is what LSP positions count by default."""
    return len(s.encode("utf-16-le")) // 2


def utf16_to_index(line: str, utf16_col: int) -> int:
    """A UTF-16 column as a Python string index."""
    return len(line.encode("utf-16-le")[: utf16_col * 2].decode("utf-16-le", "ignore"))


class Client:
    def __init__(self, root: Path, verbose: bool = False):
        self.root = root
        self.next_id = 1
        self.proc = subprocess.Popen(
            ["rust-analyzer"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=None if verbose else subprocess.DEVNULL,
            cwd=str(root),
        )

    # -- framing ---------------------------------------------------------------

    def _write(self, msg: dict) -> None:
        body = json.dumps(msg).encode()
        self.proc.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
        self.proc.stdin.flush()

    def _read(self) -> dict:
        length = None
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("rust-analyzer closed the connection")
            line = line.strip()
            if not line:
                break
            if line.lower().startswith(b"content-length:"):
                length = int(line.split(b":", 1)[1])
        if length is None:
            raise RuntimeError("LSP message with no Content-Length")
        return json.loads(self.proc.stdout.read(length))

    def _answer_if_request(self, msg: dict) -> bool:
        """Reply `null` to any server-to-client request, so the server is never left waiting."""
        if "method" in msg and "id" in msg:
            self._write({"jsonrpc": "2.0", "id": msg["id"], "result": None})
            return True
        return False

    # -- protocol --------------------------------------------------------------

    def notify(self, method: str, params: dict) -> None:
        self._write({"jsonrpc": "2.0", "method": method, "params": params})

    def request(self, method: str, params: dict, timeout: float = REQUEST_TIMEOUT_S):
        rid = self.next_id
        self.next_id += 1
        self._write({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            msg = self._read()
            if msg.get("id") == rid and ("result" in msg or "error" in msg):
                if "error" in msg:
                    raise RuntimeError(f"{method}: {msg['error'].get('message')}")
                return msg["result"]
            self._answer_if_request(msg)
        raise TimeoutError(f"{method} timed out after {timeout}s")

    def initialize(self) -> None:
        self.request(
            "initialize",
            {
                "processId": os.getpid(),
                "rootUri": uri_of(self.root),
                "workspaceFolders": [{"uri": uri_of(self.root), "name": self.root.name}],
                "capabilities": {
                    # Without documentChanges and resourceOperations the server cannot express a
                    # module rename that moves a file, and returns only the text edits -- which
                    # would leave the module renamed and its file still under the old name.
                    "workspace": {
                        "workspaceEdit": {
                            "documentChanges": True,
                            "resourceOperations": ["create", "rename", "delete"],
                        },
                        "workspaceFolders": True,
                    },
                    "textDocument": {"rename": {"prepareSupport": True}},
                    "window": {"workDoneProgress": True},
                },
            },
        )
        self.notify("initialized", {})

    def wait_until_indexed(self, timeout: float = INDEX_TIMEOUT_S) -> None:
        """Block until `rustAnalyzer/cachePriming` ends.

        `cachePriming` and nothing else. On this project the server emits, in order:
        `Building CrateGraph`, `Roots Scanned`, `Building compile-time-deps`, `Building CrateGraph`
        again, `Roots Scanned` again, `Loading proc-macros`, `Fetching`, then `cachePriming`.
        Accepting any of the earlier ones renames against a workspace whose crate graph is not
        built yet, and the server answers with an empty edit rather than an error -- which reads as
        "no references found" and would quietly do nothing.
        """
        deadline = time.monotonic() + timeout
        seen_begin = False
        while time.monotonic() < deadline:
            msg = self._read()
            if msg.get("method") == "$/progress":
                token = str(msg["params"].get("token", ""))
                kind = msg["params"].get("value", {}).get("kind")
                if "cachePriming" in token:
                    if kind == "begin":
                        seen_begin = True
                    elif kind == "end" and seen_begin:
                        return
            self._answer_if_request(msg)
        raise TimeoutError(f"rust-analyzer did not finish indexing in {timeout}s")

    def open(self, path: Path) -> None:
        self.notify(
            "textDocument/didOpen",
            {
                "textDocument": {
                    "uri": uri_of(path),
                    "languageId": "rust",
                    "version": 1,
                    "text": path.read_text(),
                }
            },
        )

    def rename(self, path: Path, line0: int, char0: int, new_name: str) -> dict:
        return self.request(
            "textDocument/rename",
            {
                "textDocument": {"uri": uri_of(path)},
                "position": {"line": line0, "character": char0},
                "newName": new_name,
            },
        )

    def shutdown(self) -> None:
        try:
            self.request("shutdown", {}, timeout=10)
            self.notify("exit", {})
            self.proc.wait(timeout=10)
        except Exception:
            self.proc.kill()


def resolve_target(root: Path, spec: str) -> tuple[Path, int, int, str]:
    """`file:line:col` (both 1-based) or `file@symbol`, as a 0-based LSP position."""
    if "@" in spec:
        file_part, symbol = spec.rsplit("@", 1)
        path = (root / file_part).resolve()
        if not path.is_file():
            sys.exit(f"no such file: {path}")
        for i, line in enumerate(path.read_text().splitlines()):
            m = re.search(rf"\b{re.escape(symbol)}\b", line)
            if m:
                return path, i, utf16_len(line[: m.start()]), symbol
        sys.exit(f"{path}: no whole-word occurrence of {symbol!r}")

    m = re.match(r"^(.*):(\d+):(\d+)$", spec)
    if not m:
        sys.exit("target must be <file>:<line>:<col> or <file>@<symbol>")
    path = (root / m.group(1)).resolve()
    if not path.is_file():
        sys.exit(f"no such file: {path}")
    line_no, col_no = int(m.group(2)), int(m.group(3))
    lines = path.read_text().splitlines()
    if not 1 <= line_no <= len(lines):
        sys.exit(f"{path} has {len(lines)} lines; asked for line {line_no}")
    line = lines[line_no - 1]
    word = re.search(r"\w+", line[col_no - 1 :])
    return path, line_no - 1, utf16_len(line[: col_no - 1]), word.group(0) if word else "?"


def apply_text_edits(path: Path, edits: list[dict]) -> None:
    text = path.read_text()
    lines = text.splitlines(keepends=True)

    def offset(pos: dict) -> int:
        line_no = pos["line"]
        base = sum(len(l) for l in lines[:line_no])
        line = lines[line_no] if line_no < len(lines) else ""
        return base + utf16_to_index(line.rstrip("\r\n"), pos["character"])

    # Latest first, so the offsets of earlier edits stay valid as the text shifts.
    for edit in sorted(edits, key=lambda e: offset(e["range"]["start"]), reverse=True):
        start, end = offset(edit["range"]["start"]), offset(edit["range"]["end"])
        text = text[:start] + edit["newText"] + text[end:]
    path.write_text(text)


def apply_workspace_edit(edit: dict, dry_run: bool) -> tuple[int, int, int]:
    """Apply a WorkspaceEdit. Returns (files edited, edits applied, paths moved)."""
    remap: dict[str, str] = {}
    files = edits = moves = 0

    def resolve(uri: str) -> str:
        # A file may be edited after it has been moved, so follow any move already applied.
        while uri in remap:
            uri = remap[uri]
        return uri

    def do_text(uri: str, text_edits: list[dict]) -> None:
        nonlocal files, edits
        path = path_of(resolve(uri))
        print(f"  {len(text_edits):3d} edit(s)  {path}")
        if not dry_run:
            apply_text_edits(path, text_edits)
        files += 1
        edits += len(text_edits)

    def do_move(old_uri: str, new_uri: str) -> None:
        nonlocal moves
        old, new = path_of(resolve(old_uri)), path_of(new_uri)
        print(f"  move {'dir ' if old.is_dir() else 'file'}  {old}  ->  {new}")
        if not dry_run:
            new.parent.mkdir(parents=True, exist_ok=True)
            shutil.move(str(old), str(new))
        remap[old_uri] = new_uri
        moves += 1

    # `documentChanges` is ordered and is the only form that can carry file moves.
    if edit.get("documentChanges"):
        for change in edit["documentChanges"]:
            kind = change.get("kind")
            if kind == "rename":
                do_move(change["oldUri"], change["newUri"])
            elif kind == "create":
                print(f"  create     {path_of(change['uri'])}")
                if not dry_run:
                    path_of(change["uri"]).touch()
            elif kind == "delete":
                print(f"  delete     {path_of(change['uri'])}")
                if not dry_run:
                    path_of(change["uri"]).unlink()
            else:
                do_text(change["textDocument"]["uri"], change["edits"])
    else:
        for uri, text_edits in (edit.get("changes") or {}).items():
            do_text(uri, text_edits)

    return files, edits, moves


def main() -> int:
    ap = argparse.ArgumentParser(
        description="Rename a Rust symbol through rust-analyzer, by meaning rather than by text.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="example:  ra-rename.py . src/lib.rs@sessions session",
    )
    ap.add_argument("workspace", help="directory holding Cargo.toml")
    ap.add_argument("target", help="<file>:<line>:<col> or <file>@<symbol>")
    ap.add_argument("new_name")
    ap.add_argument("--dry-run", action="store_true", help="show the edits, change nothing")
    ap.add_argument("--verbose", action="store_true", help="let rust-analyzer log to stderr")
    args = ap.parse_args()

    root = Path(args.workspace).resolve()
    if not (root / "Cargo.toml").is_file():
        sys.exit(f"no Cargo.toml in {root}")
    if shutil.which("rust-analyzer") is None:
        sys.exit("rust-analyzer is not on PATH (rustup component add rust-analyzer)")

    path, line0, char0, old_name = resolve_target(root, args.target)
    rel = path.relative_to(root) if path.is_relative_to(root) else path
    print(f"rename {old_name!r} -> {args.new_name!r} at {rel}:{line0 + 1}:{char0 + 1}")

    client = Client(root, args.verbose)
    try:
        client.initialize()
        print("indexing...", flush=True)
        client.wait_until_indexed()
        client.open(path)
        # Retried because an empty edit is how the server reports "not ready yet" as well as
        # "nothing to rename", and the two are indistinguishable from here. If the progress token
        # is ever renamed upstream, this is what keeps the tool working rather than silently
        # doing nothing.
        for attempt in range(3):
            edit = client.rename(path, line0, char0, args.new_name)
            if edit and (edit.get("documentChanges") or edit.get("changes")):
                break
            if attempt < 2:
                print("  no edits yet, waiting...", flush=True)
                time.sleep(3)
    finally:
        client.shutdown()

    if not edit or not (edit.get("documentChanges") or edit.get("changes")):
        print("rust-analyzer returned no edits -- is the cursor on the symbol?", file=sys.stderr)
        return 1

    print("dry run, nothing written:" if args.dry_run else "applying:")
    files, edits, moves = apply_workspace_edit(edit, args.dry_run)
    print(f"{edits} edit(s) across {files} file(s), {moves} move(s)")
    print("\nRust is done. Comments, markdown and paths in strings are not -- check those.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
