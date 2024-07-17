from rust:1.79-alpine as builder

WORKDIR /app
COPY . .

RUN apk update
RUN apk add protobuf-dev musl-dev

RUN cargo install --path .

# castle
FROM gcr.io/distroless/static-debian11 as castle

COPY --from=builder /app/target/release/castle /castle
ENTRYPOINT ["/castle"]

# castled
FROM gcr.io/distroless/static-debian11 as castled

COPY --from=builder /app/target/release/castled /castled
ENTRYPOINT ["/castled"]
