# Headless Maelstrom load-test runner for CI / Kubernetes.
# Builds ONLY the CLI + engine crates (no Tauri / no GUI), so the image is slim.
#
#   docker build -t maelstrom-cli:0.1 .
#   docker run --rm -v "$PWD:/work" maelstrom-cli:0.1 /work/scenario.json \
#       --out-json /work/report.json --max-error-rate 1 --max-p95 500

FROM rust:1-slim AS builder
WORKDIR /build

# Only the engine, DB, gRPC and CLI crates are needed — copy them and synthesize
# a minimal workspace that excludes the desktop (Tauri) crate.
COPY core ./core
COPY db ./db
COPY grpc ./grpc
COPY cli ./cli
RUN printf '[workspace]\nmembers = ["core", "db", "grpc", "cli"]\nresolver = "2"\n\n[profile.release]\nopt-level = "s"\nlto = true\nstrip = true\n' > Cargo.toml
RUN cargo build --release -p maelstrom-cli

FROM debian:stable-slim
LABEL org.opencontainers.image.title="Maelstrom CLI" \
      org.opencontainers.image.description="Headless load-test runner (HTTP, gRPC, WebSocket, databases) for CI and Kubernetes — part of Maelstrom." \
      org.opencontainers.image.source="https://github.com/slakertop1/maelstrom-releases" \
      org.opencontainers.image.url="https://github.com/slakertop1/maelstrom-releases" \
      org.opencontainers.image.documentation="https://github.com/slakertop1/maelstrom-releases"
# rustls validates server certs against the system trust store.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/maelstrom /usr/local/bin/maelstrom
# Non-root by default.
USER 65532:65532
ENTRYPOINT ["maelstrom"]
