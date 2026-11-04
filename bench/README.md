# Public benchmarks

OpenCodeReview is measured against other code reviewers on three public benchmarks. Each benchmark is scored by its own judge:

| Benchmark | What it measures | Who else is on it | How OpenCodeReview runs |
|---|---|---|---|
| [ReviewBench](https://review-bench.ai) ([repo](https://github.com/review-bench/ReviewBench)) | 219 real PRs (25 in the test set), 19 languages. Line-matched against golden findings: grounded precision and recall, plus augmented F1. | Copilot, Qodo, Greptile, Cursor, Devin, Codex, Cubic and others | The benchmark image follows the agent contract directly. Official submissions use it as is. |
| [Martian Code Review Bench](https://codereview.withmartian.com) ([repo](https://github.com/withmartian/code-review-benchmark)) | 50 PRs from Sentry, Cal.com, Grafana, Keycloak and Discourse, with 173 human-checked golden comments. Semantic match, no line anchors. | CodeRabbit, Greptile, Bugbot, Copilot, Graphite, Qodo, Augment, Cubic, Devin, Claude Code | Offline: our comments are added to their `benchmark_data.json` as one more tool, then their extract, dedup and judge steps run. |
| [AACR-Bench](https://github.com/alibaba/aacr-bench) | 200 real PRs, 10 languages, 1,505 verified issues. File-level semantic precision, recall and balanced score. | No commercial leaderboard. Baselines are the Claude Code and Codex reviewers its harness ships with. | Our results go in the shape of its `ocr` reviewer, which is Alibaba's open-code-review (a different tool); then its judge runs. |

## The benchmark image

`docker/Dockerfile.bench` builds one container that reviews one pull request and exits. It reads its inputs the way the [ReviewBench agent contract](https://github.com/review-bench/ReviewBench/blob/main/AGENT_CONTRACT.md) mounts them:

| Input | Contents |
|---|---|
| `/work/repo` | The checkout at the head commit |
| `/work/pr` | `diff.patch` and `pr.json` |
| `RB_*` | Pull request metadata (`RB_NWO`, `RB_PR_NUMBER`, `RB_BASE`, `RB_HEAD`, `RB_OUT`) |

It writes the findings to `RB_OUT`. All three adapters use this same contract.

Inside the container, `docker/bench-entrypoint.sh` starts what Compose runs as services:
- a throwaway Postgres
- one review runner
- the provider egress proxy, when the benchmark supplies no proxy of its own

It then runs `ocr-bench`, which plays the server's part: it reviews the pull request with the product's own pipeline. That covers the frozen snapshot, the sandboxed CLI under the tool guard, and evidence validation, the same way a review runs in the app, minus GitHub.

`ocr-bench` writes two files:
- `RB_OUT`: the benchmark's findings, up to 50, one per inline finding. Each message is `title. detail Suggested fix: action`. Questions are not findings and are left out.
- `ocr-findings.json`, next to `RB_OUT`: the full export. It has the engine label and version, status, tokens, duration, findings and questions. `LEFT`-side anchors are moved to the head line where the deleted lines were.

Configuration, read from the contract's `RB_CONFIG_*` variables:

| Variable | Default | Meaning |
|---|---|---|
| `RB_CONFIG_PROVIDER` | `claude` | `claude`, `codex` or `opencode`. It seeds the run's only account, so a review never fails over to another provider. |
| `RB_CONFIG_MODEL`, `RB_CONFIG_EFFORT` | the product defaults | Overrides the provider's engine choice for benchmark runs; must be a model and effort listed in `backend/src/providers/engines.rs` |
| `RB_CONFIG_DEADLINE_SECS` | 1500 | After this many seconds the review is cancelled. Two minutes before it, the review's time budget ends: the lens, output repairs and final integration batches start only while the chunk reviews and integration still required fit. Without time, a repair's invalid findings are dropped and a final batch keeps its chunk-validated findings, marked unconfirmed. The manifests set 3300 against ReviewBench's 60-minute limit. |
| `RB_CONFIG_MAX_FILES` | 800 | Settings → PR selection → Maximum PR files. The product defaults to 100; a pull request it declines scores nothing, so the benchmark allows more. |
| `RB_CONFIG_FOCUS_FILES` | 100 | The `review_focus_files` setting, which has no Settings screen yet. Above it, the review covers the highest-priority files (source code first, then the most changed lines) and lists the rest as not reviewed. `0` reviews every file; the product default is `0`. |
| `RB_CONFIG_LENS_PASS` | `true` | The `review_lens_pass` setting, which has no Settings screen yet. Each review stage gets one more execution that looks for what a first pass usually leaves out (tests, docs, maintainability, API shape, small slips) and adds only new findings; if it fails, the first report stands. It costs roughly one more execution per stage. The product default is `false`. |
| `RB_CONFIG_SUGGESTIONS` | `false` | Settings → PR selection → Review comments → Post suggestions (`review_suggestions_enabled`), with the product's default. `true` keeps the review's lower-priority suggestions, which the export then lists after the inline findings. |
| `RB_CONFIG_DIFF_RENAMES` | `false` | Benchmarks treat a moved file as new content and anchor golden findings in it, so the review diff shows renamed files as added (`diff.renames=false`). `true` keeps git's rename detection, as the product does. |

Credentials:
- **Claude:** `CLAUDE_CODE_OAUTH_TOKEN`, from `claude setup-token`.
- **Other providers:** the provider's sign-in file, either:
  - mounted at `/run/ocr-bench/credential`, or
  - named by `OCR_BENCH_CREDENTIAL_FILE`, or
  - passed inline as `OCR_BENCH_CREDENTIAL`.

`ocr-bench` reads the credential once, removes it from its environment, and hands it to the runner through the read-only sign-in projection, as the server does.

One runner per container means a pull request's chunks run one after another. Large pull requests need the 60-minute ReviewBench limit, which the manifests in `manifests/` set.

The local adapters can run the chunks side by side instead. With `OCR_BENCH_RUNNERS=N`, each pull request gets Compose's review topology, which is torn down afterwards:
- N runner containers, hardened as Compose's are, on an internal network;
- the provider egress proxy, the only way out of that network;
- the review container, with no network, reaching the runners through their sockets.

The review then takes up to N chunks at a time. The ReviewBench contract allows one container only, so an official run keeps one runner.

### Failure policy

ReviewBench is all or nothing: one pull request that keeps failing fails the whole run. So:

- **Setup problems exit non-zero.** These are a missing or rejected credential, a usage limit, or a runner that will not start. The benchmark retries them.
- **A review that fails exits 0 with no findings.** This covers failing evidence validation and exceeding `RB_CONFIG_MAX_FILES`.
- **A review the deadline cancels reports what its finished chunks validated**, with status `timeout`; files of the chunks that did not finish go unreviewed. The real status (`failed` or `timeout`) and the error are in `ocr-findings.json` and on stderr, and they count against us as missed findings.

This is a benchmark-output rule only. In the product an invalid report still fails the review and is never shown as clean.

## Build and smoke test

```bash
docker build -f docker/Dockerfile -t opencodereview-server:bench .
docker build -f docker/Dockerfile.bench --build-arg OCR_RUNTIME_IMAGE=opencodereview-server:bench -t opencodereview-bench:local .
python3 docker/bench-smoke.py      # fake CLI, no model call: one finding end to end
```

ReviewBench runs `linux/amd64`. For it, and for a submission, build both images with `--platform linux/amd64`.

## Running the benchmarks

Every adapter refuses to spend inference without `--authorize-paid-run <reviews>`. Each run directory under `bench/results/` runs once; use a new `--run-id` to repeat. Repositories are cached under `bench/cache/`. Both directories are git-ignored.

`--jobs N` reviews N pull requests at a time, each in its own container (for ReviewBench, with `--native`). It shortens a run roughly N times. With several jobs on one subscription, watch its usage limit: a review it interrupts fails as a setup problem.

```bash
export CLAUDE_CODE_OAUTH_TOKEN=...            # or OCR_BENCH_CREDENTIAL_FILE=~/.codex/auth.json

# ReviewBench: the 25-PR test set through their own try-agent.sh
git clone https://github.com/review-bench/ReviewBench ../ReviewBench
python3 bench/adapters/reviewbench.py --reviewbench-dir ../ReviewBench --set test --provider claude --authorize-paid-run 25

# Martian: 50 PRs, then their judge (needs their .env with a judge key)
git clone https://github.com/withmartian/code-review-benchmark ../code-review-benchmark
python3 bench/adapters/martian.py --martian-dir ../code-review-benchmark/offline --provider claude --authorize-paid-run 50
# Or split the set across accounts: one process per slice, each with its own token
# (CLAUDE_CODE_OAUTH_TOKEN=... python3 bench/adapters/martian.py ... --shard 1/4, then 2/4, ...)
(cd ../code-review-benchmark/offline && uv sync \
  && uv run python -m code_review_benchmark.step2_extract_comments --tool opencodereview-claude \
  && uv run python -m code_review_benchmark.step2_5_dedup_candidates --tool opencodereview-claude \
  && uv run python -m code_review_benchmark.step3_judge_comments --tool opencodereview-claude)

# AACR-Bench: a fixed 50-sample subset, then their judge
git clone https://github.com/alibaba/aacr-bench ../aacr-bench
(cd ../aacr-bench/evaluation && python -m converters.aacr_bench --limit 50 --seed 42 --validate)
python3 bench/adapters/aacr.py --aacr-dir ../aacr-bench/evaluation --provider claude --authorize-paid-run 50
(cd ../aacr-bench/evaluation && python -m pipeline run --stage eval --reviewer ocr \
  --dataset data/aacr_bench.jsonl --run-id opencodereview-claude)
```

Then compare against everyone else on the same judges:

```bash
python3 bench/report.py \
  --reviewbench-leaderboard https://review-bench.ai/api/leaderboard \
  --martian-evaluations ../code-review-benchmark/offline/results/<judge>/evaluations.json \
  --aacr-metrics opencodereview-claude=../aacr-bench/evaluation/metrics/aacr_bench/ocr/opencodereview-claude/<metrics>.json
```
