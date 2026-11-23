# The ripwire release to build; a tag of github.com/redhat-et/ripwire.
ARG RIPWIRE_VERSION=v0.6.10

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

# ripwire (https://github.com/redhat-et/ripwire), a code map tool that lets
# agents find the code for a task in few tokens. It is built on the same
# Fedora release as the runtime image, so it links the final libstdc++. On
# x86-64 it needs an x86-64-v3 CPU (AVX2), as the ripwire release binaries do.
# The agent skills are kept next to the binary; the entrypoint activates them.
FROM registry.fedoraproject.org/fedora:45 AS ripwire
ARG RIPWIRE_VERSION
RUN dnf install --assumeyes --setopt=install_weak_deps=False gcc-c++ cmake make git-core \
    && dnf clean all
RUN git clone --quiet --depth 1 --branch "${RIPWIRE_VERSION}" \
        https://github.com/redhat-et/ripwire.git /src/ripwire \
    && cmake -S /src/ripwire -B /src/ripwire/build -DCMAKE_BUILD_TYPE=Release \
    && cmake --build /src/ripwire/build --target ripwire --parallel \
    && install -D -t /out/bin /src/ripwire/build/ripwire \
    && mkdir -p /out/share/ripwire \
    && cp -R /src/ripwire/skills /out/share/ripwire/skills

FROM registry.fedoraproject.org/fedora-minimal:45
# ripwire needs `libstdc++`; its skills installer needs `bash` and `diffutils`.
RUN microdnf install -y --setopt=install_weak_deps=0 ca-certificates gh git-core openssh-clients shadow-utils \
        bash diffutils libstdc++ \
    && microdnf clean all \
    && useradd --create-home --uid 1000 mini-amaru

COPY --from=ripwire /out/bin/ripwire /usr/local/bin/ripwire
COPY --from=ripwire /out/share/ripwire/ /usr/local/share/ripwire/

WORKDIR /app
COPY --from=backend /build/target/release/mini-amaru bin/mini-amaru
COPY --from=frontend /build/dist/ web/
COPY config/runtime.yaml config/providers.yaml config/models.yaml config/routing.yaml config/
COPY skills/ skills/
COPY packaging/example.yaml projects/example.yaml
COPY packaging/launch.sh mini-amaru
COPY packaging/container-entrypoint.sh entrypoint
RUN chmod +x mini-amaru entrypoint \
    && mkdir -p data repos \
    && chown mini-amaru:mini-amaru data repos

ENV HOME=/home/mini-amaru
USER mini-amaru
EXPOSE 8765
ENTRYPOINT ["/app/entrypoint"]
CMD ["serve", "--host", "0.0.0.0", "--cors-origin", "http://127.0.0.1:8765", "--cors-origin", "http://localhost:8765"]
