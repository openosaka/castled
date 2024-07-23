package client

import (
	"fmt"

	"github.com/openosaka/castled/sdk/go/proto"
	"google.golang.org/grpc"
	// "google.golang.org/grpc-go"
)

type Client struct {
	options    *options
	grpcClient proto.TunnelServiceClient
}

type options struct {
	controlServerPort uint16
	controlServerHost string
}

func newOptions() *options {
	return &options{
		controlServerPort: 6610,
		controlServerHost: "localhost",
	}
}

type Option func(*options)

func WithControlServerHost(host string) func(*options) {
	return func(o *options) {
		o.controlServerHost = host
	}
}

func WithControlServerPort(port uint16) func(*options) {
	return func(o *options) {
		o.controlServerPort = port
	}
}

func NewClient(options ...Option) (*Client, error) {
	opts := newOptions()
	for _, o := range options {
		o(opts)
	}

	client := &Client{
		options: opts,
	}
	grpcClient, err := client.newGrpcClient()
	if err != nil {
		return nil, err
	}
	client.grpcClient = grpcClient

	return client, nil
}

func (c *Client) newGrpcClient() (proto.TunnelServiceClient, error) {
	conn, err := grpc.NewClient(fmt.Sprintf("%s:%d", c.options.controlServerHost, c.options.controlServerPort))
	if err != nil {
		return nil, err
	}
	return proto.NewTunnelServiceClient(conn), nil
}

func (c *Client) StartTunnel() error {
	return nil
}
