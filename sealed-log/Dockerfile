# built by compose.yml, from this folder
FROM rust:1.99.0-slim-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:trixie-slim
COPY --from=build /src/target/release/sealed-log /usr/local/bin/sealed-log
USER 1000:1000
ENTRYPOINT ["sealed-log", "/data", "0.0.0.0:7711"]
