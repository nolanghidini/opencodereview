"""Shared helpers for the benchmark adapters: checkouts mounted the ReviewBench way, one container
per pull request, and the paid-run gate.

Every adapter runs the benchmark image (docker/Dockerfile.bench) exactly as ReviewBench does, so
one image and one output contract serve all three benchmarks.
"""

import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import threading
import time
import uuid
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager

BENCH = pathlib.Path(__file__).resolve().parent.parent
CACHE = pathlib.Path(os.environ.get("OCR_BENCH_CACHE", BENCH / "cache"))
SHA = re.compile(r"^[0-9a-f]{40}$")
PROVIDERS = ("claude", "codex", "opencode")
# Runners per pull request. Above 1 they run beside the review in containers of their own, as
# Compose runs them, and the review's chunks run side by side.
RUNNERS = int(os.environ.get("OCR_BENCH_RUNNERS", "1"))
_repo_locks = {}
_repo_locks_guard = threading.Lock()


def log(message):
    print(message, file=sys.stderr, flush=True)


def git(repo, *args, check=True):
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        # A diff can carry bytes that are not UTF-8; they pass through unchanged.
        text=True, errors="surrogateescape", capture_output=True, env={**os.environ, "GIT_LFS_SKIP_SMUDGE": "1"},
    )
    if check and result.returncode:
        raise RuntimeError(f"git {' '.join(args[:2])} failed: {result.stderr.strip()}")
    return result.stdout.strip()


def has_commits(repo, *shas):
    return all(
        subprocess.run(["git", "-C", str(repo), "cat-file", "-e", f"{sha}^{{commit}}"], capture_output=True).returncode == 0
        for sha in shas
    )


def checkout(nwo, clone_url, base, head, pr_number=None):
    """A complete clone at `head` with `base` reachable, as ReviewBench mounts it.

    A full bare cache per repository is fetched once; each pull request gets a hardlinked local
    clone, so the container never needs the network to read objects.
    """
    if not (SHA.match(base) and SHA.match(head)):
        raise ValueError(f"{nwo}: base and head must be full commit SHAs")
    # Parallel jobs share a repository's cache: one fetch into it at a time.
    with _repo_lock(nwo):
        return _checkout(nwo, clone_url, base, head, pr_number)


def _repo_lock(nwo):
    with _repo_locks_guard:
        return _repo_locks.setdefault(nwo, threading.Lock())


def _checkout(nwo, clone_url, base, head, pr_number):
    cache = CACHE / "repos" / (nwo.replace("/", "__") + ".git")
    if not cache.exists():
        cache.parent.mkdir(parents=True, exist_ok=True)
        log(f"   cloning {clone_url} (once)")
        subprocess.run(["git", "clone", "-q", "--bare", clone_url, str(cache)], check=True)
    if not has_commits(cache, base, head):
        # Under refs of their own, so the cache never prunes them.
        refs = [f"{sha}:refs/bench/{sha}" for sha in (base, head)]
        git(cache, "fetch", "-q", "origin", *refs, check=False)
        if pr_number and not has_commits(cache, head):
            git(cache, "fetch", "-q", "origin", f"+pull/{pr_number}/head:refs/bench/pr-{pr_number}", check=False)
    if not has_commits(cache, base, head):
        raise RuntimeError(f"{nwo}: could not fetch {base[:8]} and {head[:8]}")
    # One clone per pull request, so parallel jobs on the same repository never share one.
    work = CACHE / "work" / f"{nwo.replace('/', '__')}-{head[:12]}"
    shutil.rmtree(work, ignore_errors=True)
    work.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "clone", "-q", "--local", "--no-checkout", str(cache), str(work)], check=True)
    # A shallow cache is cloned without --local, which copies only branches and tags.
    if not has_commits(work, base, head):
        git(work, "fetch", "-q", "origin", "+refs/bench/*:refs/bench/*")
    git(work, "checkout", "-q", "--detach", head)
    # The benchmark's clones have no route to the original repository.
    git(work, "remote", "remove", "origin", check=False)
    return work


def reset_cache(nwo):
    """Drops a repository's cache, so the next checkout clones it from another URL."""
    with _repo_lock(nwo):
        shutil.rmtree(CACHE / "repos" / (nwo.replace("/", "__") + ".git"), ignore_errors=True)


def release(repo):
    """Removes a pull request's clone once its review is done."""
    shutil.rmtree(repo, ignore_errors=True)


def run_all(items, jobs, work):
    """`work(index, item)` for every item, `jobs` at a time; returns the results in order.

    One container per pull request either way. Several jobs on one subscription can hit its usage
    limit, which fails the reviews it interrupts as setup problems.
    """
    if jobs <= 1:
        return [work(index, item) for index, item in enumerate(items, 1)]
    with ThreadPoolExecutor(max_workers=jobs) as pool:
        return list(pool.map(lambda pair: work(*pair), enumerate(items, 1)))


def review(image, repo, nwo, pr_number, base, head, title, body, out_dir, provider, extra_env=()):
    """Runs the image on one pull request; returns (exit code, findings.json, ocr-findings.json)."""
    pr_dir = out_dir / "pr"
    out = out_dir / "out"
    for directory in (pr_dir, out):
        shutil.rmtree(directory, ignore_errors=True)
        directory.mkdir(parents=True)
    (pr_dir / "diff.patch").write_text(
        git(repo, "diff", "--no-color", f"{base}...{head}") + "\n", errors="surrogateescape"
    )
    (pr_dir / "pr.json").write_text(json.dumps({
        "repo": f"https://github.com/{nwo}", "pr_number": int(pr_number), "base": base, "head": head,
        "nwo": nwo, "title": title or "", "body": body or "",
    }))
    command = [
        "docker", "run", "--rm",
        "-v", f"{repo}:/work/repo", "-v", f"{pr_dir}:/work/pr:ro", "-v", f"{out}:/work/out",
        "-e", f"RB_NWO={nwo}", "-e", f"RB_PR_NUMBER={pr_number}",
        "-e", f"RB_BASE={base}", "-e", f"RB_HEAD={head}",
        "-e", "RB_AGENT=opencodereview", "-e", "RB_DIFF=/work/pr/diff.patch",
        "-e", "RB_PR_JSON=/work/pr/pr.json", "-e", "RB_OUT=/work/out/findings.json",
        "-e", f"RB_CONFIG_PROVIDER={provider}",
        # The run directory (prompts, context, transcripts, rejected reports) for diagnosis. Local
        # runs only: a benchmark reads RB_OUT and nothing else from /work/out.
        "-e", "OCR_REVIEW_DEBUG_DIR=/work/out/debug",
    ]
    for name in ("RB_CONFIG_MODEL", "RB_CONFIG_EFFORT", "RB_CONFIG_MAX_FILES", "RB_CONFIG_FOCUS_FILES", "RB_CONFIG_LENS_PASS", "RB_CONFIG_SUGGESTIONS", "RB_CONFIG_DIFF_RENAMES", "RB_CONFIG_DEADLINE_SECS", "OCR_BENCH_DEADLINE_SECS"):
        if os.environ.get(name):
            command += ["-e", f"{name}={os.environ[name]}"]
    credential = os.environ.get("OCR_BENCH_CREDENTIAL_FILE")
    if credential:
        command += ["-v", f"{pathlib.Path(credential).resolve()}:/run/ocr-bench/credential:ro"]
    elif provider == "claude":
        command += ["-e", "CLAUDE_CODE_OAUTH_TOKEN"]
    started = time.monotonic()
    with runner_pool(image, RUNNERS) as pool_args, open(out_dir / "container.log", "w") as log_file:
        command += pool_args + list(extra_env) + [image]
        status = subprocess.run(command, stdout=log_file, stderr=subprocess.STDOUT).returncode
    elapsed = time.monotonic() - started

    def load(name):
        path = out / name
        return json.loads(path.read_text()) if path.is_file() else None

    log(f"   {out_dir.name}: exit {status} in {elapsed:.0f}s")
    return status, load("findings.json"), load("ocr-findings.json")


def docker(*args):
    return subprocess.run(["docker", *args], check=True, capture_output=True, text=True).stdout.strip()


@contextmanager
def runner_pool(image, count):
    """Compose's review topology for one pull request, torn down afterwards: `count` runners on an
    internal network whose only way out is the provider egress proxy. Yields the arguments that
    point the review container at them; with one runner, the container runs its own."""
    if count <= 1:
        yield []
        return
    name = f"ocr-bench-{uuid.uuid4().hex[:10]}"
    containers = []
    try:
        docker("network", "create", "--internal", name)
        for volume in ("ipc", "auth"):
            docker("volume", "create", f"{name}-{volume}")
        hardening = ["--init", "--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges:true",
                     "--pids-limit", "128", "--entrypoint", "ocr-sandbox", "--workdir", "/tmp"]
        containers.append(docker(
            "run", "-d", "--name", f"{name}-egress", "--network", name, "--network-alias", "egress",
            "--user", "10001:10001", "--memory", "128m", "--tmpfs", "/tmp:size=16m", "--tmpfs", "/data:size=1m,mode=0700",
            *hardening, image, "egress",
        ))
        docker("network", "connect", "bridge", f"{name}-egress")
        tmpfs = ["/tmp:size=64m,mode=1777", "/work:size=16m,mode=0755", "/evidence:size=512m,mode=0755",
                 "/output:size=48m,mode=0777", "/scratch:size=128m,mode=0755", "/data:size=1m,mode=0700"]
        for index in range(1, count + 1):
            containers.append(docker(
                "run", "-d", "--name", f"{name}-r{index}", "--hostname", f"r{index}", "--network", name,
                "--memory", "2g", "--memory-swap", "2g",
                *[flag for cap in ("SETUID", "SETGID", "CHOWN", "DAC_OVERRIDE", "KILL") for flag in ("--cap-add", cap)],
                *[flag for mount in tmpfs for flag in ("--tmpfs", mount)],
                "-v", f"{name}-ipc:/ipc", "-v", f"{name}-auth:/claude-auth:ro",
                "-e", "OCR_RUNNER_AUTH_ROOT=/claude-auth/app-managed", "-e", "OCR_RUNNER_PROXY=http://egress:8790",
                *hardening, image, "runner",
            ))
        sockets = ",".join(f"/ipc/r{index}.sock" for index in range(1, count + 1))
        yield ["--network", "none", "-v", f"{name}-ipc:/ipc", "-v", f"{name}-auth:/ocr/auth",
               "-e", f"OCR_BENCH_RUNNER_SOCKETS={sockets}"]
    finally:
        subprocess.run(["docker", "rm", "-f", *containers], capture_output=True)
        subprocess.run(["docker", "network", "rm", name], capture_output=True)
        subprocess.run(["docker", "volume", "rm", f"{name}-ipc", f"{name}-auth"], capture_output=True)


def authorize(run_dir, requested, authorized, provider, label, marker="authorized.json"):
    """The paid-run gate: inference is spent only with an explicit budget, once per run directory."""
    if authorized is None:
        raise SystemExit(
            f"{label} makes up to {requested} paid {provider} reviews. "
            f"Re-run with --authorize-paid-run {requested} to confirm."
        )
    if authorized < requested:
        raise SystemExit(f"--authorize-paid-run {authorized} is below the {requested} reviews this run needs")
    run_dir.mkdir(parents=True, exist_ok=True)
    marker = run_dir / marker
    try:
        with open(marker, "x") as handle:
            json.dump({"provider": provider, "reviews": requested, "at": time.time()}, handle)
    except FileExistsError:
        raise SystemExit(f"{run_dir} already ran; choose a new --run-id (or delete it to repeat on purpose)")


def check_provider(provider):
    if provider not in PROVIDERS:
        raise SystemExit(f"--provider must be one of {', '.join(PROVIDERS)}")
    if provider != "claude" and not os.environ.get("OCR_BENCH_CREDENTIAL_FILE"):
        raise SystemExit(f"Set OCR_BENCH_CREDENTIAL_FILE to the {provider} sign-in file")
    if provider == "claude" and not (os.environ.get("CLAUDE_CODE_OAUTH_TOKEN") or os.environ.get("OCR_BENCH_CREDENTIAL_FILE")):
        raise SystemExit("Set CLAUDE_CODE_OAUTH_TOKEN (claude setup-token) or OCR_BENCH_CREDENTIAL_FILE")
