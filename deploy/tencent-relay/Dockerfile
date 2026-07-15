# syntax=docker/dockerfile:1

ARG BUILDER_IMAGE=oven/bun:1.3
ARG RUST_VERSION=1.88.0

FROM ${BUILDER_IMAGE} AS builder
USER root
WORKDIR /src

RUN sed -i \
      -e 's|http://deb.debian.org/debian-security|http://mirrors.cloud.tencent.com/debian-security|g' \
      -e 's|http://deb.debian.org/debian|http://mirrors.cloud.tencent.com/debian|g' \
      /etc/apt/sources.list.d/debian.sources \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        curl \
    && rm -rf /var/lib/apt/lists/*

RUN curl --fail --location --silent --show-error \
      https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init \
      --output /tmp/rustup-init \
    && chmod 0755 /tmp/rustup-init \
    && /tmp/rustup-init -y --profile minimal --default-toolchain ${RUST_VERSION} \
    && rm -f /tmp/rustup-init

ENV PATH="/root/.cargo/bin:${PATH}"

COPY . .
RUN cargo +${RUST_VERSION} rustc --locked --release \
      --package desklink-relay --bin desklink-relay -- \
      -C target-feature=+crt-static \
    && ! readelf -l /src/target/release/desklink-relay | grep -q "Requesting program interpreter"

FROM scratch AS runtime

LABEL org.opencontainers.image.title="DeskLink Relay" \
      org.opencontainers.image.description="QUIC relay for encrypted DeskLink sessions" \
      org.opencontainers.image.version="0.1.0"

COPY --from=builder /src/target/release/desklink-relay /usr/local/bin/desklink-relay

ENV DESKLINK_RELAY_ADDR="0.0.0.0:4433" \
    DESKLINK_RELAY_SESSION_TTL_S="86400"

EXPOSE 4433/udp

USER 65534:65534
HEALTHCHECK --interval=15s --timeout=3s --start-period=5s --retries=3 \
  CMD ["/usr/local/bin/desklink-relay", "--check-config"]
ENTRYPOINT ["/usr/local/bin/desklink-relay"]
