# Slim study image for the Hermes Agent source study (issue #253).
# Builds from a pinned checkout; skips the production image's Chromium,
# Node TUI, s6 overlay and pm tool store — none are needed for CLI + cron
# + gateway experiments with an Ollama backend.
FROM python:3.14-slim-trixie

RUN apt-get -o Acquire::Retries=3 update && \
    apt-get -o Acquire::Retries=3 install -y --no-install-recommends \
        git curl ca-certificates ripgrep procps && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /opt/hermes
# Dependency layer cached unless the manifests change. The checkout's
# .dockerignore excludes *.md, so the readme pyproject references is
# touched empty here (the real one arrives with the full COPY below).
COPY pyproject.toml uv.lock ./
# Hermes pins with uv (uv.lock); pip's resolver fails on the pin graph.
RUN pip install --no-cache-dir uv && touch README.md && \
    UV_PROJECT_ENVIRONMENT=/opt/hermes/.venv uv sync --locked --no-dev --extra all --no-install-project && \
    rm README.md
ENV PATH="/opt/hermes/.venv/bin:${PATH}"

COPY . .
# The editable finder maps packages found at sync time, so the project
# itself must be (re)installed with the real source present.
RUN UV_PROJECT_ENVIRONMENT=/opt/hermes/.venv uv sync --locked --no-dev --extra all && \
    hermes --version

# State lives here; mounted per-run.
ENV HERMES_HOME=/opt/data \
    PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1
RUN mkdir -p /opt/data
VOLUME ["/opt/data"]

ENTRYPOINT ["hermes"]
