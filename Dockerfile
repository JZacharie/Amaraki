# Stage 1: Build binary using rust:latest
FROM rust:latest AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --bin aramaki

# Stage 2: Runtime image (aligned on Debian 13 / Trixie glibc with rust:latest)
FROM debian:trixie-slim AS runner
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/aramaki /usr/local/bin/aramaki

EXPOSE 3000
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:3000/health || exit 1

CMD ["/usr/local/bin/aramaki"]
