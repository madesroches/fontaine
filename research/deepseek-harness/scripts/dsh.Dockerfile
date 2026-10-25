# Build context: a checkout of deepseek-ai/deepseek-harness at 22ddee3792402c3e70ec5f86a63c3e3e895a2f60
FROM node@sha256:6449e19d8184d84062525b6a7524def03146c2cb04a7f08b076680d0e8e4240a
RUN apt-get update && apt-get install -y --no-install-recommends musl-tools git ca-certificates python3 \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /dsh
COPY . /dsh
RUN corepack enable && git init -q . 2>/dev/null || true
RUN pnpm install
RUN pnpm run build:artifacts
RUN pnpm --dir native/system run build:native
