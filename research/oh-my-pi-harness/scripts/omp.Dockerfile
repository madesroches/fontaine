# omp study image: Bun + the pinned checkout, no host mounts. Build context is the copied checkout
# (runs/omp-src), not the live clone. Natives: see the README for which addon loads.
FROM debian@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8
ARG BUN_VERSION=1.4.4
ENV BUN_INSTALL=/opt/bun PATH=/opt/bun/bin:/usr/local/bun-node-fallback-bin:/usr/local/bin:/usr/bin:/bin \
    PI_ROOT=/pi DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      git curl ca-certificates unzip tini tmux ripgrep python3 procps less build-essential \
    && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL https://bun.sh/install | bash -s "bun-v${BUN_VERSION}" && /opt/bun/bin/bun --version
RUN mkdir -p /usr/local/bun-node-fallback-bin && ln -s /opt/bun/bin/bun /usr/local/bun-node-fallback-bin/node
WORKDIR /pi
COPY --parents package.json bun.lock bunfig.toml patches/*.patch tsconfig.base.json tsconfig.json \
    packages/*/package.json packages/tsconfig.workspace.json python/robomp/web/package.json /pi/
RUN bun install --frozen-lockfile --ignore-scripts
COPY . /pi/
RUN bun --cwd=packages/coding-agent run gen:tool-views
RUN printf '#!/usr/bin/env bash\nexec bun /pi/packages/coding-agent/src/cli.ts "$@"\n' > /usr/local/bin/omp && chmod +x /usr/local/bin/omp
# Addon built from the checkout with the repo's own Dockerfile natives-builder stage (docker build --target
# natives-builder omp-src; copy /out/*.node to omp-src/packages/natives/native/ before this build).
# The last published prebuilt (18.12.6) is stale for this checkout: see the README.
COPY natives-out/pi_natives.linux-x64-modern.node /pi/packages/natives/native/
