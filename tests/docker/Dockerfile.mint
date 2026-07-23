# Linux Mint 22 is based on Ubuntu 24.04
FROM linuxmintd/mint22-amd64
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y \
    build-essential linux-headers-generic gcc make \
    python3 python3-pip python3-websockets python3-zeroconf \
    pipewire wireplumber alsa-utils \
    openssh-server dkms curl sudo \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /opt/open-apollo
COPY . .
CMD ["bash", "-c", "TEST_BUILD=1 bash scripts/install.sh --skip-init --no-dkms"]
