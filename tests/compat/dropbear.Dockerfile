FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends dropbear-bin openssh-sftp-server && rm -rf /var/lib/apt/lists/* \
    && useradd -m -s /bin/sh fixture && echo 'fixture:fixture-only-not-a-secret' | chpasswd
COPY dropbear-entrypoint.sh /entrypoint.sh
RUN chmod 755 /entrypoint.sh
ENTRYPOINT ["/bin/sh", "/entrypoint.sh"]
