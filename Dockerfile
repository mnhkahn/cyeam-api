FROM rust:1.88-slim AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && printf 'fn main() {}' > src/main.rs && cargo build --release && rm -rf src
COPY src ./src
COPY data ./data
# The dependency layer uses a placeholder main.rs. Touch the real source so
# Cargo always replaces that placeholder binary, even when Docker normalizes
# copied file timestamps.
RUN touch src/main.rs && cargo build --release

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /app/target/release/cyeam-api /cyeam-api
EXPOSE 8080
ENV PORT=8080
USER nonroot:nonroot
ENTRYPOINT ["/cyeam-api"]
