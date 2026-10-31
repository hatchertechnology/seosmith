FROM rust:1.98.1-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --locked --release
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* && useradd --uid 10001 --create-home seosmith && mkdir /data && chown 10001:10001 /data
COPY --from=build /build/target/release/seosmith /usr/local/bin/seosmith
USER 10001:10001
ENV SEOSMITH_DB=/data/seosmith.sqlite SEOSMITH_BIND=0.0.0.0:8080
EXPOSE 8080
ENTRYPOINT ["seosmith"]
CMD ["serve"]
