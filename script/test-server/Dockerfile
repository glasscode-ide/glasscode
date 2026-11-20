FROM ubuntu:24.04
RUN apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq openssh-server tmux git python3 sudo >/dev/null && mkdir -p /run/sshd
RUN useradd -m -s /bin/bash dev && echo 'dev ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/dev
COPY fake-claude /usr/local/bin/claude
COPY fake-codex /usr/local/bin/codex
COPY fake-gh /usr/local/bin/gh
COPY authorized_keys /home/dev/.ssh/authorized_keys
RUN chmod 755 /usr/local/bin/claude /usr/local/bin/codex /usr/local/bin/gh && chown -R dev:dev /home/dev/.ssh && chmod 700 /home/dev/.ssh && chmod 600 /home/dev/.ssh/authorized_keys
RUN su dev -c 'git config --global user.name dev && git config --global user.email dev@example.com && git config --global init.defaultBranch main \
 && mkdir -p /home/dev/origin && git init -q --bare /home/dev/origin/demo.git && git clone -q /home/dev/origin/demo.git /tmp/seed \
 && cd /tmp/seed && echo hi > README.md && git add -A && git commit -qm init && git push -q origin main && rm -rf /tmp/seed \
 && touch /home/dev/.glasscode-test-server'
EXPOSE 22
CMD ["/usr/sbin/sshd", "-D", "-e"]
