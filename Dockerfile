# syntax=docker/dockerfile:1

FROM --platform=$BUILDPLATFORM alpine:3.22 AS download

ARG TARGETARCH
ARG BAIDUPCS_VERSION=4.0.2

RUN set -eu; \
    apk add --no-cache ca-certificates curl unzip; \
    case "${TARGETARCH}" in \
      amd64) archive_sha256=b5f51388b510433668ca22fa5a1cb8840fb4d9725c5edc830554645010a62339 ;; \
      arm64) archive_sha256=d3568749f6475f9409b2d395466fa495133ca97f0582dbb15649a7074ed0b7fc ;; \
      *) echo "Unsupported architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    archive="BaiduPCS-Go-v${BAIDUPCS_VERSION}-linux-${TARGETARCH}.zip"; \
    curl --fail --location --retry 3 --silent --show-error --output "/tmp/${archive}" \
      "https://github.com/qjfoidnh/BaiduPCS-Go/releases/download/v${BAIDUPCS_VERSION}/${archive}"; \
    echo "${archive_sha256}  /tmp/${archive}" | sha256sum -c -; \
    unzip -p "/tmp/${archive}" "BaiduPCS-Go-v${BAIDUPCS_VERSION}-linux-${TARGETARCH}/BaiduPCS-Go" \
      > /usr/local/bin/BaiduPCS-Go; \
    chmod 755 /usr/local/bin/BaiduPCS-Go; \
    mkdir -p /rootfs/config /rootfs/downloads

FROM busybox:1.37.0-musl AS runtime-tools

FROM scratch

COPY --from=download /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=runtime-tools /bin/busybox /bin/busybox
COPY --from=download /usr/local/bin/BaiduPCS-Go /usr/local/bin/BaiduPCS-Go
COPY --from=download --chown=1000:1000 /rootfs/ /
COPY entrypoint.sh /entrypoint.sh
COPY LICENSE.BaiduPCS-Go /licenses/BaiduPCS-Go.txt

ENV HOME=/config \
    BAIDUPCS_GO_CONFIG_DIR=/config
WORKDIR /downloads

ENTRYPOINT ["/bin/busybox", "sh", "/entrypoint.sh"]
CMD ["serve"]
