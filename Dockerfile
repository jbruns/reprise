# syntax=docker/dockerfile:1
# The Service image: one static binary on distroless, run as the nonroot user. It needs no writable path but the
# store volume, so it runs with a read-only root filesystem.

FROM rust:1.99.1-alpine@sha256:b1d5dd5a22a09404d3e798c0cf051b689077360374eb505fa5bb93c479693d87 AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --package title-select \
    && cp target/release/title-select /title-select
RUN mkdir /store

FROM gcr.io/distroless/static:nonroot@sha256:18f4cc26c9ffa7f20a359298be7deadac5c607c8e03a02b8119f1a5fc2f82c99
COPY --from=build /title-select /title-select
COPY --from=build --chown=65532:65532 /store /store
ENV TITLE_SELECT_STORE=/store \
    TITLE_SELECT_LISTEN=0.0.0.0:8080
VOLUME /store
EXPOSE 8080
USER nonroot:nonroot
ENTRYPOINT ["/title-select"]
CMD ["serve"]
