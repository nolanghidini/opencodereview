FROM rust:1.98.1-bookworm AS server
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends \
  pkg-config libssl-dev \
  && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY backend ./backend
COPY assets ./assets
ARG CARGO_TARGET_CACHE=ocr-cargo-target
# Cargo reads these from the environment. Builds keep two jobs and full optimization; the CI
# smoke sets more jobs and opt-level 0, since it checks the sandbox, not speed.
ARG CARGO_BUILD_JOBS=2
ARG CARGO_PROFILE_RELEASE_OPT_LEVEL
RUN --mount=type=cache,id=ocr-cargo-registry,target=/usr/local/cargo/registry,sharing=locked --mount=type=cache,id=${CARGO_TARGET_CACHE},target=/app/target,sharing=locked cargo build --locked --release --bin ocr-server \
  && cp /app/target/release/ocr-server /usr/local/bin/

FROM debian:bookworm-slim
# Debian's security updates land after the base image is published; take them at build time.
RUN apt-get update \
  && apt-get upgrade -y --no-install-recommends \
  && apt-get install -y --no-install-recommends ca-certificates curl git bubblewrap \
  && curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg -o /usr/share/keyrings/githubcli-archive-keyring.gpg \
  && chmod go+r /usr/share/keyrings/githubcli-archive-keyring.gpg \
  && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" > /etc/apt/sources.list.d/github-cli.list \
  && apt-get update \
  && apt-get install -y --no-install-recommends gh \
  && rm -rf /var/lib/apt/lists/*
ARG TARGETARCH
ARG CLAUDE_CLI_VERSION=2.1.292
ARG CLAUDE_CLI_SHA256_AMD64=a967e7b1d8b4e47ee421d5433027880347952b0c0857abf880e2c942a4ec93b3
ARG CLAUDE_CLI_SHA256_ARM64=24caa9e6ff13bf227049a2626f1c816fc895023050f0ec3b12dbf14d897367e0
RUN case "$TARGETARCH" in \
      amd64) platform=linux-x64; checksum="$CLAUDE_CLI_SHA256_AMD64" ;; \
      arm64) platform=linux-arm64; checksum="$CLAUDE_CLI_SHA256_ARM64" ;; \
      *) echo "Unsupported Claude CLI architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac \
  && mkdir -p /opt/claude/bin \
  && curl -fsSL --retry 3 "https://storage.googleapis.com/claude-code-dist-86c565f3-f756-42ad-8dfa-d59b1c096819/claude-code-releases/${CLAUDE_CLI_VERSION}/${platform}/claude" -o /opt/claude/bin/claude \
  && printf '%s  /opt/claude/bin/claude\n' "$checksum" | sha256sum -c - \
  && chmod 0755 /opt/claude/bin/claude \
  && ln -s /opt/claude/bin/claude /usr/local/bin/claude \
  && test "$(DISABLE_AUTOUPDATER=1 /opt/claude/bin/claude --version | cut -d ' ' -f1)" = "$CLAUDE_CLI_VERSION"
LABEL ai.claude.cli.version="${CLAUDE_CLI_VERSION}"
ARG CODEX_CLI_VERSION=0.163.0
ARG CODEX_CLI_SHA256_AMD64=beb339e8ecd9ba057a6bb2dc81ad8ca474812edfbf0fc5056ca818797648dde4
ARG CODEX_CLI_SHA256_ARM64=a7261e65f4a96d8f4ca65a069c6cf2dc2c33defc341e48078acefda9461fffe4
# Codex runs its shell tool through this host for current models; without it every command fails.
ARG CODEX_CODE_MODE_HOST_SHA256_AMD64=314263128ba6592e7e807c265deeaf928a692b72dcbfeb5e0b20c9423b703529
ARG CODEX_CODE_MODE_HOST_SHA256_ARM64=284356aefb197f00b76b912efdfdbae8ccf3ea1f89b976dde064b7ccb43214ef
RUN case "$TARGETARCH" in \
      amd64) target=x86_64-unknown-linux-musl; checksum="$CODEX_CLI_SHA256_AMD64"; host_checksum="$CODEX_CODE_MODE_HOST_SHA256_AMD64" ;; \
      arm64) target=aarch64-unknown-linux-musl; checksum="$CODEX_CLI_SHA256_ARM64"; host_checksum="$CODEX_CODE_MODE_HOST_SHA256_ARM64" ;; \
      *) echo "Unsupported Codex CLI architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac \
  && mkdir -p /opt/codex/bin \
  && curl -fsSL --retry 3 "https://github.com/openai/codex/releases/download/rust-v${CODEX_CLI_VERSION}/codex-${target}.tar.gz" -o /tmp/codex.tar.gz \
  && printf '%s  /tmp/codex.tar.gz\n' "$checksum" | sha256sum -c - \
  && tar -xzf /tmp/codex.tar.gz -C /opt/codex/bin \
  && mv "/opt/codex/bin/codex-${target}" /opt/codex/bin/codex \
  && rm /tmp/codex.tar.gz \
  && curl -fsSL --retry 3 "https://github.com/openai/codex/releases/download/rust-v${CODEX_CLI_VERSION}/codex-code-mode-host-${target}.tar.gz" -o /tmp/codex-code-mode-host.tar.gz \
  && printf '%s  /tmp/codex-code-mode-host.tar.gz\n' "$host_checksum" | sha256sum -c - \
  && tar -xzf /tmp/codex-code-mode-host.tar.gz -C /opt/codex/bin \
  && mv "/opt/codex/bin/codex-code-mode-host-${target}" /opt/codex/bin/codex-code-mode-host \
  && rm /tmp/codex-code-mode-host.tar.gz \
  && chmod 0755 /opt/codex/bin/codex /opt/codex/bin/codex-code-mode-host \
  && ln -s /opt/codex/bin/codex /usr/local/bin/codex \
  && test "$(/opt/codex/bin/codex --version | cut -d ' ' -f2)" = "$CODEX_CLI_VERSION"
LABEL ai.codex.cli.version="${CODEX_CLI_VERSION}"
ARG OPENCODE_CLI_VERSION=1.19.5
ARG OPENCODE_CLI_SHA256_AMD64=588310a336fa3b9df36b3f0225b26f679d453efaad0cf3d66c3651ef3ec1647a
ARG OPENCODE_CLI_SHA256_ARM64=fae5be262d693b7c5e562856cc473e59e741772936db0c0600ac0d5ed5bd088b
RUN case "$TARGETARCH" in \
      amd64) platform=linux-x64; checksum="$OPENCODE_CLI_SHA256_AMD64" ;; \
      arm64) platform=linux-arm64; checksum="$OPENCODE_CLI_SHA256_ARM64" ;; \
      *) echo "Unsupported OpenCode CLI architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac \
  && curl -fsSL --retry 3 "https://github.com/anomalyco/opencode/releases/download/v${OPENCODE_CLI_VERSION}/opencode-${platform}.tar.gz" -o /tmp/opencode.tar.gz \
  && printf '%s  /tmp/opencode.tar.gz\n' "$checksum" | sha256sum -c - \
  && mkdir -p /opt/opencode/bin \
  && tar -xzf /tmp/opencode.tar.gz -C /opt/opencode/bin \
  && rm /tmp/opencode.tar.gz \
  && chown -R root:root /opt/opencode \
  && chmod 0755 /opt/opencode/bin/opencode \
  && ln -s /opt/opencode/bin/opencode /usr/local/bin/opencode \
  && test "$(HOME=/tmp/opencode-home XDG_DATA_HOME=/tmp/opencode-home XDG_CONFIG_HOME=/tmp/opencode-home XDG_CACHE_HOME=/tmp/opencode-home XDG_STATE_HOME=/tmp/opencode-home /opt/opencode/bin/opencode --version)" = "$OPENCODE_CLI_VERSION" \
  && rm -rf /tmp/opencode-home
LABEL ai.opencode.cli.version="${OPENCODE_CLI_VERSION}"
ENV DISABLE_AUTOUPDATER=1
WORKDIR /app
COPY --from=server /app/assets ./assets
COPY --from=server /usr/local/bin/ocr-server /usr/local/bin/ocr-server
ENV OCR_BIND=0.0.0.0:8787 \
  OCR_DATA_DIR=/data \
  OCR_REPOS_DIR=/data/repos
VOLUME ["/data"]
EXPOSE 8787
CMD ["ocr-server"]
