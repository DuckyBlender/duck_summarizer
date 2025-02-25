# Builder stage
FROM rust:latest AS builder
WORKDIR /app
# Copy manifests and source code
COPY Cargo.toml .
COPY src ./src
# Build the app in release mode
RUN cargo build --release

# Final stage
FROM debian:stable-slim
WORKDIR /app
# Copy the built binary
COPY --from=builder /app/target/release/duck_summarizer .
# Copy the .env file to the final image
COPY .env .
CMD ["./duck_summarizer"]