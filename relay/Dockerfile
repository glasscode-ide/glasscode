# Build glasscode-relay from source and run it in a tiny image.
#
#   docker build -t glasscode-relay ./relay
#   docker run -p 8080:8080 -e RELAY_SECRET=<your secret> glasscode-relay
#
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked --bin glasscode-relay

FROM debian:bookworm-slim
COPY --from=build /src/target/release/glasscode-relay /usr/local/bin/glasscode-relay
# Run as an unprivileged user.
USER 65534:65534
EXPOSE 8080
ENV LISTEN_ADDR=0.0.0.0:8080
# RELAY_SECRET must be supplied at run time; the process refuses to start without it.
ENTRYPOINT ["/usr/local/bin/glasscode-relay"]
