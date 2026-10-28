# Stack-Agent hosted image: `stack-agent serve`.
#
#   docker build -t stack-agent .
#   docker run --rm -p 8765:8765 -v "$PWD/workspaces:/workspaces" -e CHIP_PROVIDER=... stack-agent \
#       serve --host 0.0.0.0 --tenants /etc/stack-agent/tenants.json --profiles /etc/stack-agent/profiles.json
#
# What is NOT in the image: PAX. `change` and `verify` work completes only through a passing PAX test
# run, so mount or install a PAX >= 0.4.1 and put it on PATH (or set PAX_BIN); `inspect` works without
# it. Provider keys are never baked in: pass them as environment variables at run time.
#
# Built, not signed. The image is a plain build of this checkout for glibc (Debian 12); no provenance
# attestation is attached to it.

FROM rust:1.97-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p stack-agent --bin stack-agent

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates git \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --create-home --uid 10001 agent \
 && mkdir -p /workspaces /data && chown agent /workspaces /data
COPY --from=build /src/target/release/stack-agent /usr/local/bin/stack-agent
USER agent
ENV STACK_AGENT_DATA_DIR=/data
WORKDIR /workspaces
EXPOSE 8765
# Liveness: GET /health. Readiness: GET /ready (503 while draining). SIGTERM drains for
# --drain-secs (default 30) before the process exits.
STOPSIGNAL SIGTERM
ENTRYPOINT ["stack-agent"]
CMD ["serve", "--host", "0.0.0.0"]
