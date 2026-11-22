FROM docker.io/library/rust:1.98-bookworm AS backend
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
RUN cargo build --release --locked

FROM docker.io/library/node:22-trixie-slim AS frontend
WORKDIR /build
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
RUN VITE_API_URL='' npm run build

FROM registry.fedoraproject.org/fedora-minimal:45
RUN microdnf install -y --setopt=install_weak_deps=0 ca-certificates gh git-core openssh-clients shadow-utils \
    && microdnf clean all \
    && useradd --create-home --uid 1000 mini-amaru

WORKDIR /app
COPY --from=backend /build/target/release/mini-amaru bin/mini-amaru
COPY --from=frontend /build/dist/ web/
COPY config/runtime.yaml config/providers.yaml config/models.yaml config/routing.yaml config/
COPY skills/ skills/
COPY packaging/example.yaml projects/example.yaml
COPY packaging/launch.sh mini-amaru
RUN chmod +x mini-amaru \
    && mkdir -p data repos \
    && chown mini-amaru:mini-amaru data repos

ENV HOME=/home/mini-amaru
USER mini-amaru
EXPOSE 8765
ENTRYPOINT ["/app/mini-amaru"]
CMD ["serve", "--host", "0.0.0.0", "--cors-origin", "http://127.0.0.1:8765", "--cors-origin", "http://localhost:8765"]
