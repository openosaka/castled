ARG base_image=gcr.io/distroless/static-debian11

# The first stage to build all binaries
FROM golang:1.22 as build

WORKDIR /app
COPY . .
RUN mkdir /gobin && CGO_ENABLED=0 GOBIN=/gobin go install ./...

FROM $base_image AS crawler
COPY --from=build /gobin/crawler /
ENTRYPOINT ["/crawler"]
