package client

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"

	"github.com/openosaka/castled/sdk/go/proto"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/status"
)

const DEFAULT_BUFFER_SIZE = 8 * 1024

type Client struct {
	controlServerAddr string
	grpcClient        proto.TunnelServiceClient
	logger            Logger
}

type options struct {
	logger Logger
}

func newOptions() *options {
	return &options{
		logger: slog.Default(),
	}
}

type Option func(*options)

func WithLogger(logger Logger) func(*options) {
	return func(c *options) {
		c.logger = logger
	}
}

func NewClient(serverAddr string, options ...Option) (*Client, error) {
	opts := newOptions()
	for _, o := range options {
		o(opts)
	}

	client := &Client{
		logger:            opts.logger,
		controlServerAddr: serverAddr,
	}
	grpcClient, err := client.newGrpcClient()
	if err != nil {
		return nil, err
	}
	client.grpcClient = grpcClient

	return client, nil
}

func (c *Client) newGrpcClient() (proto.TunnelServiceClient, error) {
	conn, err := grpc.NewClient(c.controlServerAddr, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		return nil, err
	}
	return proto.NewTunnelServiceClient(conn), nil
}

func (c *Client) StartTunnel(ctx context.Context, tunnel *Tunnel) ([]string, <-chan error, error) {
	quit := make(chan error, 1)

	stream, err := c.grpcClient.Register(ctx, &proto.RegisterReq{
		Tunnel: &tunnel.Tunnel,
	})
	if err != nil {
		return nil, nil, fmt.Errorf("failed to register tunnel: %w", err)
	}

	command, err := stream.Recv()
	if err != nil {
		return nil, nil, fmt.Errorf("failed to init the registration: %w", err)
	}

	payload, ok := command.Payload.(*proto.ControlCommand_Init)
	if !ok {
		return nil, nil, fmt.Errorf("first command should be init")
	}

	go func() {
		defer c.logger.Debug("tunnel closed")

		var err error
		defer func() {
			quit <- err
		}()

		for {
			select {
			case <-ctx.Done():
				stream.CloseSend()
				return
			default:
			}

			var command *proto.ControlCommand
			command, err = stream.Recv()
			if gerr, ok := status.FromError(err); ok && gerr.Code() == codes.Unavailable {
				err = fmt.Errorf("control server is unavailable: %w", err)
				return
			}
			if err != nil {
				err = fmt.Errorf("failed to receive control message: %w", err)
				return
			}
			c.logger.Debug("received control message", slog.Any("command", command))

			_, ok := command.Payload.(*proto.ControlCommand_Init)
			if ok {
				err = errors.New("unexpected init command")
				return
			}

			work, ok := command.Payload.(*proto.ControlCommand_Work)
			if !ok {
				err = errors.New("unexpected command, expected work command")
				return
			}

			//TODO(sword): traffic control
			go func() {
				// TODO(sword): refactor using the tunnel to handle protocol specific
				if err := c.work(ctx, tunnel.LocalAddr, work); err != nil {
					c.logger.Error("failed to process work command", slog.Any("error", err))
				}
			}()
		}
	}()

	return payload.Init.AssignedEntrypoint, quit, nil
}

func (c *Client) work(ctx context.Context, localAddr string, work *proto.ControlCommand_Work) error {
	connectionID := work.Work.ConnectionId

	bidiStream, err := c.grpcClient.Data(ctx)
	if err != nil {
		return fmt.Errorf("failed to create data stream: %w", err)
	}

	localConn, err := net.Dial("tcp", localAddr)
	if err != nil {
		err2 := bidiStream.Send(&proto.TrafficToServer{
			ConnectionId: connectionID,
			Action:       proto.TrafficToServer_Close,
		})
		if err2 != nil {
			c.logger.Error("failed to send close action to control server, the server maybe crashed", slog.Any("error", err2))
		}

		return fmt.Errorf("failed to dial to local address: %w", err)
	}

	if err := bidiStream.Send(&proto.TrafficToServer{
		ConnectionId: connectionID,
		Action:       proto.TrafficToServer_Start,
	}); err != nil {
		return fmt.Errorf("failed to send start action: %w", err)
	}

	go func() {
		// read from the stream
		defer func() {
			c.logger.Debug("quit reading")
			if tcpConn, ok := localConn.(*net.TCPConn); ok {
				tcpConn.CloseWrite()
			}
		}()

		for {
			select {
			case <-ctx.Done():
				return
			default:
			}

			dataToClient, err := bidiStream.Recv()
			if err == io.EOF || len(dataToClient.Data) == 0 {
				c.logger.Debug("server closed the stream, most of times are because the server finished the work")
				return
			}
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
				ConnectionId: connectionID,
				Action:       proto.TrafficToServer_Sending,
				Data:         buf[:n],
			}); err != nil {
				c.logger.Error("failed to send data to control server", slog.Any("error", err))
				break
			}
		}

		if err := bidiStream.Send(&proto.TrafficToServer{
			ConnectionId: connectionID,
			Action:       proto.TrafficToServer_Finished,
		}); err != nil {
			c.logger.Error("failed to send close action to control server", slog.Any("error", err))
		}
	}()

	return nil
}
