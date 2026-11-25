# syntax=docker/dockerfile:1
ARG NODE_VERSION
FROM node:${NODE_VERSION}-bookworm-slim AS node

FROM ubuntu:22.04 AS toolchain
ARG RUST_VERSION=1.91.1
ENV DEBIAN_FRONTEND=noninteractive CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup PATH=/usr/local/cargo/bin:$PATH
RUN apt-get update && apt-get install -y --no-install-recommends build-essential ca-certificates curl file libssl-dev perl pkg-config \
    && rm -rf /var/lib/apt/lists/*
RUN curl --fail --silent --show-error https://sh.rustup.rs -o /tmp/rustup-init.sh \
    && sh /tmp/rustup-init.sh -y --profile minimal --default-toolchain "$RUST_VERSION" \
    && rm /tmp/rustup-init.sh
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm && ln -s ../lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx

FROM toolchain AS source
WORKDIR /src
COPY . .
ARG APP_DIR
RUN --mount=type=cache,target=/root/.npm cd "$APP_DIR" && npm ci

FROM toolchain AS linux-build
RUN apt-get update && apt-get install -y --no-install-recommends libayatana-appindicator3-dev librsvg2-dev libwebkit2gtk-4.1-dev libxdo-dev patchelf xdg-utils \
    && rm -rf /var/lib/apt/lists/*
COPY --from=source /src /src
WORKDIR /src
ENV CARGO_TARGET_DIR=/src/target QEMU_GUEST_BASE=0x800000000000
ARG APP_DIR
ARG APP_VERSION
ARG RELEASE_CONFIG
ARG OVERRIDE_CONFIG
RUN --mount=type=cache,id=veruna-pack-tauri-linux-target,target=/src/target,sharing=locked \
    --mount=type=cache,id=veruna-pack-tauri-registry,target=/usr/local/cargo/registry \
    --mount=type=secret,id=tauri_signing_private_key,required=true \
    --mount=type=secret,id=tauri_signing_password,required=true \
    rustc -vV && rm -rf /src/target/release/bundle && cd "$APP_DIR" \
    && export TAURI_SIGNING_PRIVATE_KEY="$(cat /run/secrets/tauri_signing_private_key)" \
    && export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$(cat /run/secrets/tauri_signing_password)" \
    && set -- && if [ -n "$RELEASE_CONFIG" ]; then set -- --config "$RELEASE_CONFIG"; fi \
    && VITE_APP_VERSION="$APP_VERSION" npm exec -- tauri build --ci "$@" --config "$OVERRIDE_CONFIG" --bundles deb -- --locked \
    && mkdir -p /artifacts && cp -R /src/target/release/bundle /artifacts/bundle

FROM toolchain AS windows-build
RUN apt-get update && apt-get install -y --no-install-recommends clang cmake lld llvm ninja-build nsis python3 \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add x86_64-pc-windows-msvc
# Microsoft's channel endpoint can close Rustls connections before a response.
# The supported native TLS backend keeps system certificate validation enabled.
RUN cargo install --locked --version 0.23.1 --no-default-features --features native-tls,xz2/static cargo-xwin
ENV XWIN_ARCH=x86_64 XWIN_CACHE_DIR=/xwin CARGO_TARGET_DIR=/src/target LANG=C.UTF-8 LC_ALL=C.UTF-8
COPY --from=source /src /src
WORKDIR /src
ARG APP_DIR
ARG APP_VERSION
ARG RELEASE_CONFIG
ARG OVERRIDE_CONFIG
ARG TARGETARCH
RUN --mount=type=cache,id=veruna-pack-tauri-windows-target-${TARGETARCH},target=/src/target,sharing=locked \
    --mount=type=cache,id=veruna-pack-tauri-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=veruna-pack-tauri-xwin,target=/xwin,sharing=locked \
    --mount=type=secret,id=tauri_signing_private_key,required=true \
    --mount=type=secret,id=tauri_signing_password,required=true \
    rm -rf /src/target/x86_64-pc-windows-msvc/release/bundle && cd "$APP_DIR" \
    && export TAURI_SIGNING_PRIVATE_KEY="$(cat /run/secrets/tauri_signing_private_key)" \
    && export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$(cat /run/secrets/tauri_signing_password)" \
    && set -- && if [ -n "$RELEASE_CONFIG" ]; then set -- --config "$RELEASE_CONFIG"; fi \
    && VITE_APP_VERSION="$APP_VERSION" npm exec -- tauri build --ci "$@" --config "$OVERRIDE_CONFIG" --runner cargo-xwin --target x86_64-pc-windows-msvc -- --locked \
    && mkdir -p /artifacts && cp -R /src/target/x86_64-pc-windows-msvc/release/bundle /artifacts/bundle

FROM scratch AS linux-artifacts
COPY --from=linux-build /artifacts/ /
FROM scratch AS windows-artifacts
COPY --from=windows-build /artifacts/ /
