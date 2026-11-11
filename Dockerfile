FROM rust:1.93-alpine AS chef
RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static
RUN cargo install cargo-chef
WORKDIR /app

FROM chef AS planner
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
RUN cargo chef prepare --recipe-path recipe.json

FROM node:24-alpine AS frontend-builder
WORKDIR /app/admin-ui
COPY admin-ui/package.json admin-ui/pnpm-lock.yaml ./
# pnpm 10.x 会把 ignored build scripts 当作错误（ERR_PNPM_IGNORED_BUILDS）。
# @swc/core 的平台专用二进制通过 optional dependencies 提供，
# 不依赖 postinstall 脚本，--ignore-scripts 在容器构建中安全且更快。
# --frozen-lockfile：按锁文件安装，镜像构建可复现（Rust 侧同样带 Cargo.lock）
RUN npm install -g pnpm && pnpm install --frozen-lockfile --ignore-scripts
COPY admin-ui ./
RUN pnpm build

FROM chef AS builder

# 可选：启用敏感日志输出（仅用于排障）
ARG ENABLE_SENSITIVE_LOGS=false

COPY --from=planner /app/recipe.json recipe.json
RUN if [ "$ENABLE_SENSITIVE_LOGS" = "true" ]; then \
        cargo chef cook --release --features sensitive-logs --recipe-path recipe.json; \
    else \
        cargo chef cook --release --recipe-path recipe.json; \
    fi

COPY Cargo.toml Cargo.lock* ./
COPY src ./src
COPY --from=frontend-builder /app/admin-ui/dist /app/admin-ui/dist

RUN if [ "$ENABLE_SENSITIVE_LOGS" = "true" ]; then \
        cargo build --release --features sensitive-logs; \
    else \
        cargo build --release; \
    fi

FROM alpine:3.21

RUN apk add --no-cache ca-certificates

WORKDIR /app
COPY --from=builder /app/target/release/kiro-rs /app/kiro-rs

VOLUME ["/app/config"]

EXPOSE 8990

CMD ["./kiro-rs", "-c", "/app/config/config.json", "--credentials", "/app/config/credentials.json"]
