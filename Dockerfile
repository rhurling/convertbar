# 1: web assets
FROM node:24-bookworm-slim AS web
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci
COPY tsconfig*.json vite.config.ts index.html ./
COPY public ./public
COPY src ./src
RUN npx tsc && VITE_HEAD=server npx vite build --outDir dist-web --emptyOutDir

# 2: server binary
FROM rust:1-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY src-tauri/Cargo.toml ./src-tauri/Cargo.toml
# src-tauri is a workspace member but is NOT built; give cargo a stub lib so the
# workspace parses without the tauri sources or GUI deps:
RUN mkdir -p src-tauri/src && echo "" > src-tauri/src/lib.rs
COPY --from=web /app/dist-web ./dist-web
RUN cargo build --release -p convertbar-server

# 3: runtime
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends handbrake-cli ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/convertbar-server /usr/local/bin/convertbar-server
ENV CONVERTBAR_DATA_DIR=/config
VOLUME /config
EXPOSE 8080
ENTRYPOINT ["convertbar-server"]
