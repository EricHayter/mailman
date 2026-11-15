FROM rust:1-slim-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked && cp target/release/mailman /mailman

FROM debian:trixie-slim
COPY --from=build /mailman /usr/local/bin/mailman
USER 65532
EXPOSE 3000
ENTRYPOINT ["mailman"]
