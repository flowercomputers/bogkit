# Build from the workspace root as context: `fold` (and its own `anny` path
# dep) are sibling path dependencies, so the whole workspace has to be in
# the build context, not just this crate's directory.
#
#   docker build -f examples/mmo-cs/Dockerfile -t mmo-cs .

FROM rust:1-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p mmo-cs

FROM debian:bookworm-slim
COPY --from=builder /app/target/release/mmo-cs /usr/local/bin/mmo-cs
ENV MMO_PORT=8080
EXPOSE 8080
CMD ["mmo-cs"]
