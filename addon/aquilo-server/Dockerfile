# Build context is the REPOSITORY ROOT, not this folder, so the build can reach
# the Rust workspace. Build it with:
#
#   docker buildx build -f addon/aquilo-server/Dockerfile \
#     --platform linux/arm64 \
#     --build-arg BUILD_FROM=ghcr.io/home-assistant/aarch64-base-debian:bookworm \
#     -t ghcr.io/<owner>/aarch64-aquilo-server:<version> .
#
# (CI does this per-arch — see .github/workflows/addon.yml.)
ARG BUILD_FROM=ghcr.io/home-assistant/amd64-base-debian:bookworm

# --- build stage: cross-compile the release binary on the NATIVE build host ---
# Pinned to $BUILDPLATFORM so cargo always runs on amd64 at native speed — no
# QEMU emulation of the compiler (which was the ~40 min bottleneck). The target
# arch is selected from $TARGETARCH/$TARGETVARIANT and produced with the matching
# Debian cross toolchain. Everything here is bookworm-based, so the resulting
# binary links against the same glibc as the runtime base below.
FROM --platform=$BUILDPLATFORM rust:1-bookworm AS builder

ARG TARGETARCH
ARG TARGETVARIANT

# Cross C/linker toolchains for the non-native targets. `ring` (pulled in via
# rustls) compiles C and assembly, so it needs a full cross-compiler, not just a
# cross-linker.
RUN apt-get update && apt-get install -y --no-install-recommends \
        gcc-aarch64-linux-gnu \
        gcc-arm-linux-gnueabihf \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY aquilo-core ./aquilo-core
COPY aquilo-store ./aquilo-store
COPY aquilo-server ./aquilo-server

# Map the Docker target arch to a Rust triple + cross toolchain, build, then lift
# the binary out of the (cache-mounted, non-persisted) target dir to a fixed path.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target,id=aquilo-target-${TARGETARCH}${TARGETVARIANT} \
    set -eux; \
    case "${TARGETARCH}/${TARGETVARIANT}" in \
      "amd64/"*) target=x86_64-unknown-linux-gnu ;; \
      "arm64/"*) target=aarch64-unknown-linux-gnu; \
        export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc; \
        export CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc; \
        export AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar ;; \
      "arm/v7") target=armv7-unknown-linux-gnueabihf; \
        export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER=arm-linux-gnueabihf-gcc; \
        export CC_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-gcc; \
        export AR_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-ar ;; \
      *) echo "unsupported target ${TARGETARCH}/${TARGETVARIANT}" >&2; exit 1 ;; \
    esac; \
    rustup target add "$target"; \
    cargo build --release --locked --bin aquilo-server --target "$target"; \
    cp "target/$target/release/aquilo-server" /aquilo-server

# --- runtime stage: Home Assistant base image (provides bashio + s6) ---
# Only COPY steps run here, so this stage needs no target-arch execution.
FROM ${BUILD_FROM}
COPY --from=builder /aquilo-server /usr/bin/aquilo-server
COPY --chmod=755 addon/aquilo-server/run.sh /run.sh
CMD [ "/run.sh" ]
