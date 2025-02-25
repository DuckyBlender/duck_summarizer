# Builder stage
FROM rust:latest as builder
WORKDIR /app
# Copy manifests and source code
COPY Cargo.toml .
COPY src ./src
# Build the app in release mode
RUN cargo build --release

# Final stage
FROM debian:bullseye-slim
WORKDIR /app
# Copy the built binary
COPY --from=builder /app/target/release/duck_summarizer .
RUN ["./duck_summarizer"]
