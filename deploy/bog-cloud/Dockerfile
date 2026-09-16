FROM rust:1.95.0-slim-bookworm AS source
RUN apt-get update && apt-get install -y --no-install-recommends build-essential cmake pkg-config ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
ARG BOG_BUILD_COMMIT=development
ENV BOG_BUILD_COMMIT=$BOG_BUILD_COMMIT
FROM source AS build
RUN cargo build --locked --release -p bog-cloud-records --bin bog-records-worker -p bog-cloud --bin bog-cloud-admin -p bog-cloud-mcp --bin bog-cloud-server

FROM source AS verify
RUN cargo build --locked -p bog-cloud-records --bin bog-records-worker
RUN cargo test --locked -p fold -p bog-serve -p bog-cloud-records -p bog-cloud -p bog-cloud-mcp

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates gosu && rm -rf /var/lib/apt/lists/* && useradd --uid 10001 --create-home bog
COPY --from=build /src/target/release/bog-records-worker /src/target/release/bog-cloud-admin /src/target/release/bog-cloud-server /usr/local/bin/
COPY deploy/bog-cloud/entrypoint.sh /usr/local/bin/bog-entrypoint
ENV BOG_CLOUD_ROOT=/data/bog BOG_WORKER_BINARY=/usr/local/bin/bog-records-worker BOG_CLOUD_BIND=0.0.0.0:8080
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/bog-entrypoint"]
CMD ["bog-cloud-server"]
