# syntax=docker/dockerfile:1

# Builds a static musl binary. aws-lc (the TLS crypto library) needs a C toolchain.
FROM rust:1.99-alpine AS build
RUN apk add --no-cache build-base cmake perl
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p kobodl-web \
    && cp target/release/kobodl-web /kobodl-web
# The data directory has to exist and belong to the nonroot user, so named volumes mounted there are writable.
RUN mkdir /data

# distroless/static has CA certificates, which the HTTPS client reads, and a nonroot user. No shell.
FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /kobodl-web /usr/local/bin/kobodl-web
COPY --from=build --chown=nonroot:nonroot /data /data
ENV KOBODL_LISTEN=0.0.0.0:8080 \
    KOBODL_DATA_DIR=/data
VOLUME /data
EXPOSE 8080
USER nonroot
ENTRYPOINT ["/usr/local/bin/kobodl-web"]
