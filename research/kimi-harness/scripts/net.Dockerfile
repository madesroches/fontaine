FROM debian@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8
RUN apt-get update && apt-get install -y --no-install-recommends socat tcpdump ca-certificates \
    && rm -rf /var/lib/apt/lists/*
