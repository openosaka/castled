package client

import (
	"context"
	"fmt"
	"io"
	"log/slog"
	"net"

	"github.com/openosaka/castled/sdk/go/proto"
	"google.golang.org/grpc"
)

const DEFAULT_BUFFER_SIZE = 8 * 1024

type Client struct {
	controlServerAddr string
	grpcClient        proto.TunnelServiceClient
	logger            Logger
}

type options struct {
	controlServerPort uint16
	controlServerHost string
	logger            Logger
}

func newOptions() *options {
	return &options{
		controlServerPort: 6610,
		controlServerHost: "localhost",
		logger:            slog.Default(),
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

func WithLogger(logger Logger) func(*options) {
	return func(c *options) {
		c.logger = logger
	}
}

func NewClient(options ...Option) (*Client, error) {
	opts := newOptions()
	for _, o := range options {
		o(opts)
	}

	client := &Client{
		logger:            opts.logger,
		controlServerAddr: fmt.Sprintf("%s:%d", opts.controlServerHost, opts.controlServerPort),
	}
	grpcClient, err := client.newGrpcClient()
	if err != nil {
		return nil, err
	}
	client.grpcClient = grpcClient

	return client, nil
}

func (c *Client) newGrpcClient() (proto.TunnelServiceClient, error) {
	conn, err := grpc.NewClient(c.controlServerAddr)
	if err != nil {
		return nil, err
	}
	return proto.NewTunnelServiceClient(conn), nil
}

func (c *Client) StartTunnel(ctx context.Context, tunnel *Tunnel) ([]string, error) {
	stream, err := c.grpcClient.Register(ctx, &proto.RegisterReq{
		Tunnel: &tunnel.Tunnel,
	})
	if err != nil {
		return nil, fmt.Errorf("failed to register tunnel: %w", err)
	}

	command, err := stream.Recv()
	if err != nil {
		return nil, fmt.Errorf("failed to receive control message: %w", err)
	}

	payload, ok := command.Payload.(*proto.ControlCommand_Init)
	if !ok {
		return nil, fmt.Errorf("first command should be init")
	}

	go func() {
		for {
			select {
			case <-ctx.Done():
				stream.CloseSend()
				return
			default:
			}

			command, err := stream.Recv()
			if err != nil {
				c.logger.Error("failed to receive control message", slog.Any("error", err))
				return
			}
			c.logger.Debug("received control message", slog.Any("command", command))

			_, ok := command.Payload.(*proto.ControlCommand_Init)
			if ok {
				c.logger.Error("unexpected init command")
				return
			}

			work, ok := command.Payload.(*proto.ControlCommand_Work)
			if !ok {
				c.logger.Error("unexpected command, expected work command")
				return
			}

			//TODO(sword): traffic control
			go func() {
				if err := c.work(ctx, tunnel.LocalAddr, work); err != nil {
					c.logger.Error("failed to process work command", slog.Any("error", err))
				}
			}()
		}
	}()

	return payload.Init.AssignedEntrypoint, nil
}

func (c *Client) work(ctx context.Context, localAddr string, work *proto.ControlCommand_Work) error {
	bidiStream, err := c.grpcClient.Data(ctx)
	if err != nil {
		return fmt.Errorf("failed to create data stream: %w", err)
	}

	localConn, err := net.Dial("tcp", localAddr)
	if err != nil {
		err2 := bidiStream.Send(&proto.TrafficToServer{
			Action: proto.TrafficToServer_Close,
		})
		if err2 != nil {
			c.logger.Error("failed to send close action to control server, the server maybe crashed", slog.Any("error", err2))
		}

		return fmt.Errorf("failed to dial to local address: %w", err)
	}

	if err := bidiStream.Send(&proto.TrafficToServer{
		Action: proto.TrafficToServer_Start,
	}); err != nil {
		return fmt.Errorf("failed to send start action: %w", err)
	}

	go func() {
		// read from the stream
		defer func() {
			c.logger.Debug("quit reading")
		}()

		for {
			select {
			case <-ctx.Done():
				return
			default:
			}

			dataToClient, err := bidiStream.Recv()
			if err != nil {
				c.logger.Error("failed to receive data", slog.Any("error", err))
				return
			}

			n, err := localConn.Write(dataToClient.Data)
			if err != nil {
				c.logger.Error("failed to write data to local connection", slog.Any("error", err))
				return
			}
			c.logger.Debug("wrote data to local connection", slog.Int("n", n))
		}
	}()

	go func() {
		// write to the stream
		defer func() {
			c.logger.Debug("quit writing")
		}()

		for {
			select {
			case <-ctx.Done():
				return
			default:
			}

			buf := make([]byte, DEFAULT_BUFFER_SIZE)
			n, err := localConn.Read(buf)
			if err == io.EOF {
				c.logger.Debug("no more data to read from local connection")
				break
			}
			if err != nil {
				c.logger.Error("failed to read data from local connection", slog.Any("error", err))
				return
			}
			c.logger.Debug("read data from local connection", slog.Int("n", n))

			if err := bidiStream.Send(&proto.TrafficToServer{
				ConnectionId: work.Work.ConnectionId,
				Action:       proto.TrafficToServer_Sending,
				Data:         buf[:n],
			}); err != nil {
				c.logger.Error("failed to send data to control server", slog.Any("error", err))
				break
			}
		}

		if err := bidiStream.Send(&proto.TrafficToServer{
			Action: proto.TrafficToServer_Finished,
		}); err != nil {
			c.logger.Error("failed to send close action to control server", slog.Any("error", err))
		}
	}()

	return nil
}
