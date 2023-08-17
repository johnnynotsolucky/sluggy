FROM debian:stable-slim
RUN apt-get update -y
RUN apt-get install -y libssl-dev openssl
RUN mkdir -p /usr/local/bin
COPY ./target/release/sluggy /usr/local/bin/sluggy
WORKDIR /usr/src/sluggy/
