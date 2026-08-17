# sod-web: three-stage build. Context is the REPO ROOT (the addon
# path-depends on sod/ and fold/):
#   docker build -f examples/sod-web/Dockerfile .
#   fly deploy . -c examples/sod-web/fly.toml

# ── stage 1: the Rust addon ─────────────────────────────────────────
FROM rust:1 AS addon
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY fold fold
COPY anny anny
COPY ese ese
COPY sod sod
COPY examples examples
RUN cargo build -p sod-web-addon --release

# ── stage 2: the Next.js build ──────────────────────────────────────
FROM node:22 AS web
WORKDIR /app
COPY examples/sod-web/package.json examples/sod-web/package-lock.json ./
COPY examples/sod-web/addon/package.json examples/sod-web/addon/index.js ./addon/
RUN npm ci
COPY examples/sod-web/ .
COPY --from=addon /src/target/release/libsod_web_addon.so ./addon/sod_web_addon.node
RUN npm run build

# ── stage 3: runtime ────────────────────────────────────────────────
FROM node:22-slim
WORKDIR /app
ENV NODE_ENV=production
ENV SOD_DATA_DIR=/data/sod
# Next's standalone server binds process.env.HOSTNAME when present, and
# Fly machines inject HOSTNAME=<machine-id> — pin it or the proxy can't
# reach the app
ENV HOSTNAME="0.0.0.0"
COPY --from=web /app/.next/standalone ./
COPY --from=web /app/.next/static ./.next/static
# the addon must resolve at runtime as a real package next to server.js
COPY --from=web /app/addon ./node_modules/sod-web-addon
EXPOSE 3000 7300
CMD ["node", "server.js"]
