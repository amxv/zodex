# syntax=docker/dockerfile:1.7

FROM ubuntu:22.04 AS builder

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

ARG DEBIAN_FRONTEND=noninteractive

ENV CARGO_HOME=/usr/local/cargo
ENV RUSTUP_HOME=/usr/local/rustup
ENV PATH=/usr/local/cargo/bin:${PATH}

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
      build-essential \
      ca-certificates \
      curl \
      git \
      libssl-dev \
      pkg-config && \
    rm -rf /var/lib/apt/lists/*

RUN curl https://sh.rustup.rs -sSf | sh -s -- -y --profile minimal --default-toolchain stable

WORKDIR /workspace

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests

RUN cargo build --locked --release \
      --bin zodex-client \
      --bin zodex \
      --bin zodexd \
      --bin zodex-prd

FROM ubuntu:22.04

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

ARG DEBIAN_FRONTEND=noninteractive
ARG NODE_MAJOR=22
ARG GO_VERSION=1.24.1
ARG RUST_TOOLCHAIN=stable
ARG TARGETARCH

LABEL org.opencontainers.image.source="https://github.com/amxv/zodex"
LABEL org.opencontainers.image.description="Generic zodex dev/runtime image for standard Linux VPS and container environments."
LABEL org.opencontainers.image.licenses="MIT"

ENV CARGO_HOME=/usr/local/cargo
ENV RUSTUP_HOME=/usr/local/rustup
ENV PATH=/usr/local/cargo/bin:/usr/local/go/bin:${PATH}

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
      ca-certificates \
      curl \
      gnupg && \
    rm -rf /var/lib/apt/lists/*

RUN mkdir -p /etc/apt/keyrings /usr/share/keyrings && \
    curl -fsSL https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key | gpg --dearmor -o /etc/apt/keyrings/nodesource.gpg && \
    echo "deb [signed-by=/etc/apt/keyrings/nodesource.gpg] https://deb.nodesource.com/node_${NODE_MAJOR}.x nodistro main" > /etc/apt/sources.list.d/nodesource.list && \
    curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg | dd of=/usr/share/keyrings/githubcli-archive-keyring.gpg && \
    chmod go+r /usr/share/keyrings/githubcli-archive-keyring.gpg && \
    echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" > /etc/apt/sources.list.d/github-cli.list && \
    apt-get update && \
    apt-get install -y --no-install-recommends \
      bash-completion \
      build-essential \
      bzip2 \
      ca-certificates \
      cmake \
      curl \
      dnsutils \
      fd-find \
      file \
      gh \
      git \
      git-lfs \
      gnupg \
      htop \
      iproute2 \
      iputils-ping \
      jq \
      less \
      libssl-dev \
      locales \
      make \
      nano \
      net-tools \
      nodejs \
      openssh-client \
      pkg-config \
      procps \
      psmisc \
      python-is-python3 \
      python3 \
      python3-dev \
      python3-pip \
      python3-venv \
      ripgrep \
      rsync \
      silversearcher-ag \
      socat \
      sqlite3 \
      sudo \
      tar \
      tmux \
      tree \
      unzip \
      vim \
      wget \
      xz-utils \
      zip \
      zsh && \
    rm -rf /var/lib/apt/lists/*

RUN case "${TARGETARCH}" in \
      amd64) GO_ARCH="amd64" ;; \
      arm64) GO_ARCH="arm64" ;; \
      *) echo "unsupported TARGETARCH: ${TARGETARCH}"; exit 1 ;; \
    esac && \
    curl -fsSL "https://go.dev/dl/go${GO_VERSION}.linux-${GO_ARCH}.tar.gz" | tar -C /usr/local -xz

RUN curl https://sh.rustup.rs -sSf | sh -s -- -y --profile minimal --default-toolchain "${RUST_TOOLCHAIN}" && \
    rustup component add clippy rustfmt && \
    python3 -m pip install --no-cache-dir --upgrade pip && \
    python3 -m pip install --no-cache-dir uv && \
    git lfs install --system && \
    ln -sf /usr/bin/fdfind /usr/local/bin/fd && \
    mkdir -p /workspace /etc/zodex /var/lib/zodex

COPY --from=builder /workspace/target/release/zodex-client /usr/local/bin/zodex-client
COPY --from=builder /workspace/target/release/zodex /usr/local/bin/zodex
COPY --from=builder /workspace/target/release/zodexd /usr/local/bin/zodexd
COPY --from=builder /workspace/target/release/zodex-prd /usr/local/bin/zodex-prd

RUN chmod 0755 /usr/local/bin/zodex-client \
               /usr/local/bin/zodex \
               /usr/local/bin/zodexd \
               /usr/local/bin/zodex-prd

WORKDIR /workspace

EXPOSE 443 8080

CMD ["/bin/bash"]
