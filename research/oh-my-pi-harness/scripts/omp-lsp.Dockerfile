# Adds a Python language server (Debian's python3-pylsp) on top of the study image for the LSP leg of E4.
FROM omp-study:dcafcef6
USER root
RUN apt-get update && apt-get install -y --no-install-recommends python3-pylsp python3-pyflakes python3-setuptools && rm -rf /var/lib/apt/lists/* && which pylsp
