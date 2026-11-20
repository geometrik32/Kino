# One image with both binaries (kino-server, kino-bot) and the built web app.

FROM rust:1-bookworm AS build
ARG DX_VERSION=0.7.10
RUN rustup target add wasm32-unknown-unknown \
 && curl -fsSL "https://github.com/DioxusLabs/dioxus/releases/download/v${DX_VERSION}/dx-x86_64-unknown-linux-gnu.tar.gz" \
    | tar xz -C /usr/local/bin
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p kino-server -p kino-bot
RUN cd app && dx bundle --web --release \
 && mkdir -p /out \
 && cp -r "$(dirname "$(find /src/target/dx -path '*release/web/public/index.html' | head -n1)")" /out/web

FROM debian:bookworm-slim
# ffmpeg re-encodes what browsers cannot play; the Intel driver lets it use
# Quick Sync / VA-API on the server's UHD 630 (pass /dev/dri to the container).
RUN sed -i 's/Components: main/Components: main non-free non-free-firmware/' /etc/apt/sources.list.d/debian.sources \
 && apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates ffmpeg intel-media-va-driver-non-free \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/kino-server /src/target/release/kino-bot /usr/local/bin/
COPY --from=build /out/web /srv/web
ENV STATIC_DIR=/srv/web BIND_ADDR=0.0.0.0:8080
EXPOSE 8080
USER nobody
CMD ["kino-server"]
