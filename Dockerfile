# Multi-stage build for the `adler` CLI.
# Build:  docker build -t adler .
# Run:    docker run --rm adler alice

# Builder and runtime must share a Debian release so glibc versions match.
FROM rust:1-slim-bookworm@sha256:c8a94a78f67ec8c4d474ec7f71e0720f21eb7e584e158daec0874cafa7c30e4d AS builder
WORKDIR /src
# Copy the whole workspace; .dockerignore keeps target/ and the cache out.
COPY . .
RUN cargo build --release -p adler-cli

FROM debian:bookworm-slim@sha256:60eac759739651111db372c07be67863818726f754804b8707c90979bda511df AS runtime
# reqwest uses rustls, but TLS still needs the system root certificates.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/adler /usr/local/bin/adler
# Cache lives here; mount a volume to persist it across runs.
ENV XDG_CACHE_HOME=/tmp/cache
ENTRYPOINT ["adler"]
CMD ["--help"]
