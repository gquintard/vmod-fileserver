FROM rust:1.61-buster

WORKDIR /vmod_fileserver
ARG VMOD_FILESERVER_VERSION=0.0.1
ARG RELEASE_URL=https://github.com/gquintard/vmod_fileserver/archive/refs/tags/v${VMOD_FILESERVER_VERSION}.tar.gz

RUN curl -s https://packagecloud.io/install/repositories/varnishcache/varnish72/script.deb.sh | bash && apt-get update && apt-get install -y varnish-dev clang libssl-dev

RUN curl -Lo dist.tar.gz ${RELEASE_URL} && \
	tar xavf dist.tar.gz --strip-components=1 && \
	cargo build --release

FROM varnish:7.2
COPY --from=0 /vmod_fileserver/target/release/libvmod_fileserver.so /usr/lib/varnish/vmods/
