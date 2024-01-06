FROM rust:1
WORKDIR /usr/src/notion-ics
COPY . .

RUN cargo install --path .

CMD ["notion-ics"]
