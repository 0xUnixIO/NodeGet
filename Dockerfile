# syntax=docker/dockerfile:1

ARG ALPINE_VERSION=3.22
ARG RUST_IMAGE=rust:alpine3.22

FROM ${RUST_IMAGE} AS chef

RUN apk add --no-cache build-base clang clang-dev git musl-dev pkgconf \
    && cargo install cargo-chef --locked

WORKDIR /src

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS source-binary

COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --profile minimal --recipe-path recipe.json

COPY . .
RUN cargo build --package nodeget-server --profile minimal --locked \
    && mkdir -p /out \
    && cp target/minimal/nodeget-server /out/nodeget-server \
    && chmod 0755 /out/nodeget-server

FROM alpine:${ALPINE_VERSION} AS runtime-base

LABEL org.opencontainers.image.title="NodeGet Server"
LABEL org.opencontainers.image.description="NodeGet server runtime image based on Alpine Linux"
LABEL org.opencontainers.image.source="https://github.com/wynn/NodeGet"
LABEL org.opencontainers.image.licenses="AGPL-3.0"

RUN apk add --no-cache ca-certificates tzdata \
    && mkdir -p /etc/nodeget /var/lib/nodeget

COPY docker/entrypoint.sh /usr/local/bin/nodeget-entrypoint

RUN chmod 0755 /usr/local/bin/nodeget-entrypoint

WORKDIR /etc/nodeget

ENV NODEGET_PORT="2211" \
    NODEGET_LOG_FILTER="info" \
    NODEGET_CONFIG_PATH="/etc/nodeget/config.toml" \
    NODEGET_DATABASE_URL="sqlite:///var/lib/nodeget/nodeget.db?mode=rwc"

EXPOSE 2211

ENTRYPOINT ["/usr/local/bin/nodeget-entrypoint"]
CMD ["serve"]

FROM runtime-base AS runtime-source
COPY --from=source-binary /out/nodeget-server /usr/local/bin/nodeget-server

FROM runtime-source AS runtime
