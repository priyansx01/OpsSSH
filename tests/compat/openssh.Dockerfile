FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends openssh-server openssh-sftp-server tmux && rm -rf /var/lib/apt/lists/* \
    && useradd -m -s /bin/bash fixture && echo 'fixture:fixture-only-not-a-secret' | chpasswd \
    && rm -f /etc/ssh/ssh_host_*
COPY sshd_config /etc/ssh/sshd_config
COPY entrypoint.sh /entrypoint.sh
RUN chmod 755 /entrypoint.sh
ENTRYPOINT ["/bin/sh", "/entrypoint.sh"]
