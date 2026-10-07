FROM alpine:3.23
RUN apk add --no-cache openssh-server openssh-sftp-server tmux \
    && adduser -D -s /bin/ash fixture && echo 'fixture:fixture-only-not-a-secret' | chpasswd
COPY sshd_config /etc/ssh/sshd_config
COPY entrypoint.sh /entrypoint.sh
RUN chmod 755 /entrypoint.sh
ENTRYPOINT ["/bin/sh", "/entrypoint.sh"]
