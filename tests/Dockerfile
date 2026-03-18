FROM rust:latest
RUN apt-get update && apt-get install -y cmake build-essential perl pkg-config curl python3 jq procps && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY . .
RUN cargo build --release && cp target/release/nyrun /usr/local/bin/nyrun
COPY tests/integration.sh /tests/integration.sh
COPY tests/fixtures /tests/fixtures
RUN chmod +x /tests/integration.sh /tests/fixtures/http-server.py
RUN mkdir -p /var/run/nyrun
WORKDIR /
ENTRYPOINT ["/tests/integration.sh"]
