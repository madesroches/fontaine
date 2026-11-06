# Installs the published npm package, not a source build: the pinned commit's package.json carries the same version (2.5.1).
FROM node@sha256:994ed664b2644b1b186516daaa1fb7aa93a1e717da2a0ff17094c765d5dbfde1
RUN apt-get update && apt-get install -y --no-install-recommends git ca-certificates python3 ripgrep \
    && rm -rf /var/lib/apt/lists/*
RUN npm install -g @moonshot-ai/kimi-code@2.5.1 && kimi --version
