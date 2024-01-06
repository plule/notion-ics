FROM rust:1
WORKDIR /usr/src/notion-ics
COPY . .

RUN cargo install --path .

ENTRYPOINT ["notion-ics"]
