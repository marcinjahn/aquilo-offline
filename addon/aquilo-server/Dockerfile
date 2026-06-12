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

# Cross linkers for the non-native targets. The dependency tree is pure Rust (TLS
# is disabled — see the workspace Cargo.toml), so we don't need a C cross-compiler,
# but linking against glibc still needs the target's C-runtime objects (Scrt1.o,
# crti.o, libc) from libc6-dev-*-cross. gcc-arm-linux-gnueabihf, unlike its arm64
# counterpart, does not pull that in, so we install both explicitly.
RUN apt-get update && apt-get install -y --no-install-recommends \
        gcc-aarch64-linux-gnu \
        gcc-arm-linux-gnueabihf \
        libc6-dev-arm64-cross \
        libc6-dev-armhf-cross \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY aquilo-core ./aquilo-core
COPY aquilo-store ./aquilo-store
COPY aquilo-server ./aquilo-server

# Map the Docker target arch to a Rust triple + cross linker, build, then lift the
# binary out of the (cache-mounted, non-persisted) target dir to a fixed path.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target,id=aquilo-target-${TARGETARCH}${TARGETVARIANT} \
    set -eux; \
    case "${TARGETARCH}/${TARGETVARIANT}" in \
      "amd64/"*) target=x86_64-unknown-linux-gnu ;; \
      "arm64/"*) target=aarch64-unknown-linux-gnu; \
        export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc ;; \
      "arm/v7") target=armv7-unknown-linux-gnueabihf; \
        export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER=arm-linux-gnueabihf-gcc ;; \
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
