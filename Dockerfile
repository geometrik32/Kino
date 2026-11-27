# One image with both binaries (kino-server, kino-bot) and the built web app.

# dx needs glibc 2.39+, so Debian 13 (trixie).
FROM rust:1-trixie AS build
ARG DX_VERSION=0.7.10
ARG DX_SHA256=4363e4ed2a3f1eb7f4d38d2d59aed59ce43271c44c16b425e92c89a64761fbe7
RUN apt-get update \
 && apt-get install -y --no-install-recommends brotli \
 && rm -rf /var/lib/apt/lists/* \
 && rustup target add wasm32-unknown-unknown \
 && curl -fsSL "https://github.com/DioxusLabs/dioxus/releases/download/v${DX_VERSION}/dx-x86_64-unknown-linux-gnu.tar.gz" -o /tmp/dx.tar.gz \
 && echo "${DX_SHA256}  /tmp/dx.tar.gz" | sha256sum -c - \
 && tar xzf /tmp/dx.tar.gz -C /usr/local/bin \
 && rm -f /tmp/dx.tar.gz
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p kino-server -p kino-bot
RUN cd app && dx bundle --web --release -p kino-app \
 && mkdir -p /out \
 && cp -r "$(dirname "$(find /src/target/dx -path '*release/web/public/index.html' | head -n1)")" /out/web \
 && perl -0777 -i -pe 'if (!/viewport-fit=/i) { s{(<meta\b[^>]*\bname=["\x27]?viewport["\x27]?[^>]*>)}{ my $t = $1; $t =~ s{(\bcontent=["\x27])([^"\x27]*)}{$1 . ($2 ? "$2, " : "") . "viewport-fit=cover"}ie; $t }ige }' /out/web/index.html \
 && grep -qi 'viewport-fit=cover' /out/web/index.html \
 && find /out/web -type f \( -name '*.wasm' -o -name '*.js' -o -name '*.css' -o -name '*.html' -o -name '*.svg' \) \
    -exec sh -c 'gzip -9 -k "$1" && brotli -9 -k "$1"' _ {} \;

FROM debian:trixie-slim
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
