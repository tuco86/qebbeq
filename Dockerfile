# syntax=docker/dockerfile:1.6

FROM rust:1.89-slim AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./

# build dependencies
RUN \
    mkdir -p src \
    && echo "fn main(){}" > src/main.rs \
    && cargo build --release \
    && ls target \
    && rm -rf src

# Build real binary
COPY src ./src
RUN \
    touch src/main.rs \
    && cargo build --release

# Collect binary and its dynamic deps from the same (trixie) environment
RUN set -e; \
        mkdir -p /out && \
        cp -v target/release/qebbeq /out/qebbeq && \
                # Copy all absolute-path deps (including the dynamic loader) reported by ldd
                ldd target/release/qebbeq | awk '{for (i=1;i<=NF;i++) if ($i ~ /^\//) print $i}' | sort -u | \
                    xargs -r -I '{}' cp -v --parents '{}' /out && \
        # NSS bits for DNS resolution
        mkdir -p /out/etc && \
        [ -f /etc/nsswitch.conf ] && cp -v /etc/nsswitch.conf /out/etc/nsswitch.conf || true && \
        for f in /lib/x86_64-linux-gnu/libnss_files.so.2 /lib/x86_64-linux-gnu/libnss_dns.so.2; do \
            [ -f "$f" ] && cp -v --parents "$f" /out || true; \
        done ; \
        ls /out

########################################
# Final: scratch with glibc runtime files
########################################
FROM scratch AS final

# Copy binary and required libs from builder (same glibc/trixie)
COPY --from=builder /out/ /

# Run as non-root (65532:65532)
USER 65532:65532

ENV RUST_LOG=info
ENTRYPOINT ["/qebbeq"]
