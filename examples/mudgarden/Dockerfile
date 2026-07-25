# syntax=docker/dockerfile:1

FROM rust:1.97.1-bookworm AS builder

WORKDIR /app
COPY . .

RUN cargo build --locked --release -p mudgarden \
    && cp target/release/mudgarden /tmp/mudgarden

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /data

COPY --from=builder /tmp/mudgarden /usr/local/bin/mudgarden

WORKDIR /data

ENV MUDGARDEN_BIND=0.0.0.0:2222

EXPOSE 2222

CMD ["/usr/local/bin/mudgarden", "serve"]
