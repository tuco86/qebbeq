# syntax=docker/dockerfile:1.6

FROM rust:1.89-slim AS builder
WORKDIR /app

ENV CRANE_VERSION=0.20.6

# Install curl
RUN \
    apt-get update \
    && apt-get install -y curl \
    && rm -rf /var/lib/apt/lists/*

# NSS bits for DNS resolution
RUN set -e; \
    mkdir -p /out/etc && \
    [ -f /etc/nsswitch.conf ] && cp -v /etc/nsswitch.conf /out/etc/nsswitch.conf || true && \
    for f in /lib/*-linux-gnu/libnss_files.so.2 /lib/*-linux-gnu/libnss_dns.so.2; do \
        [ -f "$f" ] && cp -v --parents "$f" /out || true; \
    done

# Install CA-certificates
RUN set -e; \
    mkdir -p /out/etc/ssl/certs \
    && cp -v /etc/ssl/certs/ca-certificates.crt /out/etc/ssl/certs/ca-certificates.crt

# Install crane (linux amd64 static)
RUN \
    mkdir -p /out/usr/bin \
    && arch=$(dpkg --print-architecture) \
    && curl -fsSL "https://github.com/google/go-containerregistry/releases/download/v${CRANE_VERSION}/go-containerregistry_Linux_${arch}.tar.gz" | tar -xz -C /out/usr/bin crane \
    && chown root:root /out/usr/bin/crane

COPY Cargo.toml Cargo.lock ./

# build dependencies
RUN \
    mkdir -p src \
    && echo "fn main(){}" > src/main.rs \
    && cargo build --release \
    && rm -rf src

# Build real binary
COPY src ./src
RUN \
    touch src/main.rs \
    && cargo build --release

# Collect binary and its dynamic deps from the same (trixie) environment
RUN \
    cp -v target/release/qebbeq /out/usr/bin/qebbeq \
    # Copy all absolute-path deps (including the dynamic loader) reported by ldd
    && ldd /out/usr/bin/qebbeq \
        | awk '{for (i=1;i<=NF;i++) if ($i ~ /^\//) print $i}' \
        | sort -u \
        | xargs -r -I '{}' cp -v --parents '{}' /out

########################################
# Final: scratch with glibc runtime files
########################################
FROM scratch AS final

# Copy binary and required libs from builder (same glibc/trixie)
COPY --from=builder /out/ /

# Run as non-root (65532:65532)
USER 65532:65532

ENV RUST_LOG=info

ENTRYPOINT ["qebbeq"]
