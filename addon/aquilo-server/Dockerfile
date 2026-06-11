# Build context is the REPOSITORY ROOT, not this folder, so the build can reach
# the Rust workspace. Build it with:
#
#   docker buildx build -f addon/aquilo-server/Dockerfile \
#     --build-arg BUILD_FROM=ghcr.io/home-assistant/amd64-base-debian:bookworm \
#     -t ghcr.io/<owner>/amd64-aquilo-server:<version> .
#
# (CI does this per-arch under qemu — see .github/workflows/addon.yml.)
ARG BUILD_FROM=ghcr.io/home-assistant/amd64-base-debian:bookworm

# --- build stage: compile the release binary for the target platform ---
FROM rust:1-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY aquilo-core ./aquilo-core
COPY aquilo-store ./aquilo-store
COPY aquilo-server ./aquilo-server
RUN cargo build --release --locked --bin aquilo-server

# --- runtime stage: Home Assistant base image (provides bashio + s6) ---
FROM ${BUILD_FROM}
COPY --from=builder /src/target/release/aquilo-server /usr/bin/aquilo-server
COPY addon/aquilo-server/run.sh /run.sh
RUN chmod a+x /run.sh
CMD [ "/run.sh" ]
