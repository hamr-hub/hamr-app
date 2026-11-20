FROM rust:1.82-slim AS builder
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release && rm -rf src
COPY src ./src
COPY migrations ./migrations
COPY skills ./skills
RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/hamr-app-server .
COPY --from=builder /app/migrations ./migrations
# 场景包：版本化不可变的 SKILL.md，运行时按 HAMR_AGENT_SKILLS_DIR 读取
COPY --from=builder /app/skills ./skills
EXPOSE 3002
CMD ["./hamr-app-server"]
