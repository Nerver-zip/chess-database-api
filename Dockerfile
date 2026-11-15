FROM rust:1.100.0-bookworm@sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential pkg-config libzstd-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --locked --release --package codb-cli

FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl libgcc-s1 libstdc++6 libzstd1 \
    && groupadd --system --gid 10001 codb \
    && useradd --system --uid 10001 --gid codb --home-dir /nonexistent --shell /usr/sbin/nologin codb \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder --chown=10001:10001 /workspace/target/release/codb /usr/local/bin/codb

ENV RUST_LOG=info
USER 10001:10001
EXPOSE 8080
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=10s --timeout=3s --start-period=90s --retries=5 \
    CMD ["/usr/bin/curl", "--fail", "--silent", "--show-error", "http://127.0.0.1:8080/ready"]
ENTRYPOINT ["/usr/local/bin/codb"]
