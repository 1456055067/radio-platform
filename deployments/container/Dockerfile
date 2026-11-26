# syntax=docker/dockerfile:1.7
# Multi-stage build for the gateway (with the web console) and the site
# agent. Build from the repository root:
#   docker buildx build -f deployments/container/Dockerfile --target gateway -t radio-gateway .
#   docker buildx build -f deployments/container/Dockerfile --target agent -t radio-agent .
# Multi-arch: add --platform linux/amd64,linux/arm64 (CI builds each
# architecture natively; see .github/workflows/images.yml).
# Pin the base images by digest in production (Renovate/Dependabot can
# keep the digests current).

ARG RUST_VERSION=1.100.0
ARG NODE_VERSION=22

FROM docker.io/library/node:${NODE_VERSION}-bookworm-slim AS ui
WORKDIR /src/frontend
COPY frontend/package.json frontend/package-lock.json frontend/.npmrc ./
# .npmrc sets ignore-scripts=true: no dependency install hooks run.
RUN npm ci --no-audit --no-fund
COPY frontend/ ./
RUN npm run build

FROM docker.io/library/rust:${RUST_VERSION}-bookworm AS build
ARG RUST_VERSION
# libopus is compiled from source (cmake); aws-lc-sys needs a C compiler.
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake \
 && rm -rf /var/lib/apt/lists/*
# Use the image's toolchain rather than whatever rust-toolchain.toml resolves.
ENV RUSTUP_TOOLCHAIN=${RUST_VERSION} CARGO_TERM_COLOR=never
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p api-gateway -p scanner-agent \
 && mkdir /out \
 && cp target/release/api-gateway target/release/scanner-agent /out/

# Runtime: distroless (glibc, CA certificates, no shell, no package
# manager), non-root user 65532.
FROM gcr.io/distroless/cc-debian12:nonroot AS gateway
LABEL org.opencontainers.image.title="radio-platform gateway" \
      org.opencontainers.image.licenses="Apache-2.0"
COPY --from=build /out/api-gateway /usr/local/bin/api-gateway
COPY --from=ui /src/frontend/dist /usr/share/radio-platform/ui
USER 65532:65532
EXPOSE 8443 9443 9464
ENTRYPOINT ["/usr/local/bin/api-gateway"]
CMD ["/etc/radio/gateway.toml"]

FROM gcr.io/distroless/cc-debian12:nonroot AS agent
LABEL org.opencontainers.image.title="radio-platform site agent" \
      org.opencontainers.image.licenses="Apache-2.0"
COPY --from=build /out/scanner-agent /usr/local/bin/scanner-agent
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/scanner-agent"]
CMD ["/etc/radio/agent.toml"]
