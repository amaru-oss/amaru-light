# The ripwire release to build; a tag of github.com/redhat-et/ripwire.
ARG RIPWIRE_VERSION=v0.6.10

FROM docker.io/library/rust:1.98-bookworm AS backend
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY prompts/ prompts/
RUN cargo build --release --locked --bin amaru

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
COPY --from=backend /build/target/release/amaru bin/amaru
COPY examples/ examples/
COPY packaging/container-entrypoint.sh entrypoint
# Mount the settings at /config and keep the state (database, mirrors,
# worktrees) on a volume at /data: set `state_dir: /data` in amaru.yaml.
RUN chmod +x entrypoint \
    && mkdir -p /config /data \
    && chown mini-amaru:mini-amaru /config /data

ENV HOME=/home/mini-amaru
USER mini-amaru
VOLUME ["/data"]
ENTRYPOINT ["/app/entrypoint"]
CMD ["--config", "/config/amaru.yaml", "serve"]
