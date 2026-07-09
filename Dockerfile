# Static musl build → scratch image. Final image is ~6 MB: the binary and nothing else.
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN cargo build --release --bin stoke && \
    strip target/release/stoke

FROM scratch
COPY --from=build /src/target/release/stoke /stoke
# Config is mounted at runtime; auth is required unless STOKE_DEV=1.
#   docker run -p 8787:8787 -e STOKE_API_KEYS=yourkey \
#     -v $PWD/stoke.toml:/stoke.toml ghcr.io/ozperium/stoke
EXPOSE 8787
ENTRYPOINT ["/stoke"]
