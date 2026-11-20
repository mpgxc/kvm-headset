# Hub image for the Orange Pi (linux/arm64).
FROM rust:1-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends cmake libasound2-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p headhub-hub

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends libasound2 alsa-utils \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/headhub-hub /usr/local/bin/headhub-hub
ENTRYPOINT ["headhub-hub"]
