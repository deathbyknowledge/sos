LATEST_VERSION=$(curl -s "https://api.github.com/repos/containers/crun/releases/latest" | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/')

sudo wget https://github.com/containers/crun/releases/download/${LATEST_VERSION}/crun-${LATEST_VERSION}-linux-amd64 -O /tmp/crun
sudo mv /tmp/crun /usr/local/bin/crun
sudo chmod +x /usr/local/bin/crun

# Verify installation
crun --version

