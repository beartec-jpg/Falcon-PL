# Multi-stage build for falcon-dagd (needs C toolchain for pqcrypto-falcon).
FROM rust:1.85-bookworm AS build
WORKDIR /src
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential clang cmake pkg-config \
    && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p fd-node

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/falcon-dagd /usr/local/bin/falcon-dagd
RUN mkdir -p /data /keys
VOLUME ["/data"]
EXPOSE 18001 19001
ENTRYPOINT ["falcon-dagd"]
CMD ["--id", "val1", "--rpc", "0.0.0.0:18001", "--unl", "val1", "--data-dir", "/data"]
