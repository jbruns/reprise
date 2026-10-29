# syntax=docker/dockerfile:1
# The Service image: one static binary and the workbench's bundle on distroless, run as the nonroot user. It needs no
# writable path but the store volume, so it runs with a read-only root filesystem.

FROM rust:1.99.1-alpine@sha256:b1d5dd5a22a09404d3e798c0cf051b689077360374eb505fa5bb93c479693d87 AS build
ARG TARGETARCH
RUN apk add --no-cache musl-dev binaryen
# cargo-leptos builds the workbench; it runs the wasm-bindgen it finds on PATH, which must match Cargo.lock's.
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) arch=x86_64; \
        leptos_sha=149b88535fd8f44321da0d667fecc445c15af35f62931b2091687445297cc9e0; \
        bindgen_sha=82d12bb940e2d4e72e0d5605387fc1b8ca179044e012b620f0ce4e7440e8320e ;; \
      arm64) arch=aarch64; \
        leptos_sha=834d34b7bc837ed4bfdc5a26428f7c7b249550a5aecd5e6220b76b702008ab12; \
        bindgen_sha=2ed4351c35dd9440308bbb02767d47ea278efe851a52465300f3c94f5b6c2a87 ;; \
      *) echo "unsupported architecture $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    leptos="cargo-leptos-$arch-unknown-linux-musl"; \
    bindgen="wasm-bindgen-0.2.129-$arch-unknown-linux-musl"; \
    wget -q "https://github.com/leptos-rs/cargo-leptos/releases/download/v0.3.11/$leptos.tar.gz"; \
    wget -q "https://github.com/wasm-bindgen/wasm-bindgen/releases/download/0.2.129/$bindgen.tar.gz"; \
    printf '%s  %s\n' "$leptos_sha" "$leptos.tar.gz" "$bindgen_sha" "$bindgen.tar.gz" | sha256sum -c -; \
    tar xzf "$leptos.tar.gz"; tar xzf "$bindgen.tar.gz"; \
    install "$leptos/cargo-leptos" "$bindgen/wasm-bindgen" /usr/local/cargo/bin/; \
    rm -r "$leptos"* "$bindgen"*; \
    rustup target add wasm32-unknown-unknown
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo leptos build --release --precompress --cargo-locked \
    && cp target/release/title-select /title-select \
    && cp -r target/site /ui
RUN mkdir /store

FROM gcr.io/distroless/static:nonroot@sha256:18f4cc26c9ffa7f20a359298be7deadac5c607c8e03a02b8119f1a5fc2f82c99
COPY --from=build /title-select /title-select
COPY --from=build /ui /ui
COPY --from=build --chown=65532:65532 /store /store
ENV TITLE_SELECT_STORE=/store \
    TITLE_SELECT_LISTEN=0.0.0.0:8080 \
    TITLE_SELECT_UI=/ui
VOLUME /store
EXPOSE 8080
USER nonroot:nonroot
ENTRYPOINT ["/title-select"]
CMD ["serve"]
